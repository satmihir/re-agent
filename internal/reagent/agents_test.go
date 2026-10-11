package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

type agentTestModel struct {
	generate func(context.Context, ModelRequest) (ModelResponse, error)
}

func (agentTestModel) Name() string { return "test" }
func (m agentTestModel) Generate(ctx context.Context, req ModelRequest) (ModelResponse, error) {
	return m.generate(ctx, req)
}

func agentFixture(t *testing.T) *Session {
	t.Helper()
	t.Setenv("HOME", t.TempDir())
	ws, err := OpenWorkspace(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	tools := append(NewAgentTools(), NewReadFileTool(ws), NewListFilesTool(ws), NewSearchTextTool(ws), NewWriteFileTool(ws), NewExecTool(ws))
	registry, err := NewRegistry(Mode{}, tools...)
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Provider: openaiName, Model: "gpt-6-sol", Workspace: ws, WorkspacePath: ws.Root(), Registry: registry, Agents: true}
	s := NewSession(cfg, NewScriptedModel(), NewTrace(io.Discard), io.Discard)
	s.agentModel = func(Config, *Trace) (Model, error) { return NewScriptedModel(turn(textBlock("child done"))), nil }
	t.Cleanup(s.closeAgents)
	return s
}

func agentTurn(t *testing.T, s *Session, text string) RunResult {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	result, err := s.Turn(ctx, text, NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
	if err != nil || result.Status != StatusCompleted {
		t.Fatalf("turn: %+v %v", result, err)
	}
	return result
}

func spawnBlock(id, task, name string) OutputBlock {
	args, _ := json.Marshal(spawnArgs{Task: task, Name: name, Context: &agentContext{Brief: "inspect independently"}})
	return callBlock(id, "spawn", string(args))
}

func agentResults(history []Entry) []agentResult {
	var results []agentResult
	for _, e := range history {
		if e.User == nil || e.User.Source != "agent" {
			continue
		}
		_, raw, ok := strings.Cut(e.User.Text, "\n")
		if !ok {
			continue
		}
		var result agentResult
		if json.Unmarshal([]byte(raw), &result) == nil && result.ID != "" {
			results = append(results, result)
		}
	}
	return results
}

func TestAgents_ParallelChildrenFinishAtBoundary(t *testing.T) {
	s := agentFixture(t)
	started := make(chan struct{}, 2)
	release := make(chan struct{})
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
			started <- struct{}{}
			select {
			case <-release:
			case <-ctx.Done():
				return ModelResponse{}, ctx.Err()
			}
			return turn(textBlock("independent result")), nil
		}}, nil
	}
	step := 0
	s.model = agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
		step++
		switch step {
		case 1:
			return turn(spawnBlock("a", "one", ""), spawnBlock("b", "two", "")), nil
		case 2:
			for i := 0; i < 2; i++ {
				select {
				case <-started:
				case <-ctx.Done():
					t.Error("children did not run in parallel")
				}
			}
			if len(agentResults(req.History)) != 0 {
				t.Error("result inserted into an in-flight request")
			}
			time.AfterFunc(30*time.Millisecond, func() { close(release) })
			return turn(textBlock("premature final")), nil
		default:
			found := false
			for _, e := range req.History {
				if e.User != nil && strings.Contains(e.User.Text, "t1, t2 still running") {
					found = true
				}
			}
			if !found || len(agentResults(req.History)) != 2 {
				t.Errorf("finish rule or delivery missing: %+v", req.History)
			}
			return turn(textBlock("finished after results")), nil
		}
	}}
	result := agentTurn(t, s, "parallel")
	if result.Reply != "finished after results" || len(s.agentRoster()) != 0 || s.Turns() != 1 {
		t.Fatalf("result %+v roster %+v turns %d", result, s.agentRoster(), s.Turns())
	}
	// The whole batch is answered before any independently completed result.
	firstResult := -1
	lastTool := -1
	for i, e := range s.history {
		if e.Tool != nil {
			lastTool = i
		}
		if e.User != nil && e.User.Source == "agent" && strings.Contains(e.User.Text, "Agent result") && firstResult < 0 {
			firstResult = i
		}
	}
	if firstResult <= lastTool {
		t.Fatal("result interleaved with batch")
	}
}

func TestAgents_NestedDepthCapAndParentLocalControl(t *testing.T) {
	s := agentFixture(t)
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			task := req.History[0].User.Text
			if req.Scope.Step == 1 {
				switch {
				case strings.HasPrefix(task, "depth-1"):
					return turn(spawnBlock("nest", "depth-2", "second")), nil
				case strings.HasPrefix(task, "depth-2"):
					return turn(spawnBlock("nest", "depth-3", "third")), nil
				default:
					return turn(spawnBlock("too-deep", "depth-4", "fourth"), callBlock("sideways", "send", `{"id":"t1","message":"not mine"}`)), nil
				}
			}
			return turn(textBlock("nested done")), nil
		}}, nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("top", "depth-1", "first")), turn(callBlock("wait", "wait", `{"ids":["first"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "nested")
	ttree := s.agents
	ttree.mu.Lock()
	if len(ttree.nodes) != 3 {
		t.Errorf("tree size %d", len(ttree.nodes))
	}
	paths := make([]string, 0)
	for _, n := range ttree.nodes {
		if n.depth == 3 {
			paths = append(paths, n.TracePath)
		}
	}
	ttree.mu.Unlock()
	if len(paths) != 1 {
		t.Fatal("depth-3 agent did not run")
	}
	foundDepth, foundScope := false, false
	for _, e := range readEvents(t, paths[0]) {
		if e.Type == "tool.finished" {
			data := e.Data.(map[string]any)
			out := data["outcome"].(map[string]any)
			foundDepth = foundDepth || strings.Contains(fmt.Sprint(out["message"]), "maximum agent depth")
			foundScope = foundScope || strings.Contains(fmt.Sprint(out["message"]), "belongs to this parent")
		}
	}
	if !foundDepth || !foundScope {
		t.Fatal("depth/scope limits missing")
	}
}

func TestAgents_CapacityFailsWithoutWaitingAndNamesAreUnique(t *testing.T) {
	s := agentFixture(t)
	var batch []OutputBlock
	for i := 0; i < 9; i++ {
		batch = append(batch, spawnBlock(fmt.Sprintf("s%d", i), "work", fmt.Sprintf("agent-%d", i)))
	}
	s.model = NewScriptedModel(turn(batch...), turn(textBlock("done")), turn(textBlock("done")))
	agentTurn(t, s, "capacity")
	got := results(s)
	if len(got) != 9 || got[8].Outcome.OK || !strings.Contains(got[8].Outcome.Message, "at capacity: wait for or dismiss an agent") {
		t.Fatalf("capacity: %+v", got)
	}
	if len(s.agentRoster()) != 8 {
		t.Fatal("wrong live capacity")
	}
	s.model = NewScriptedModel(turn(callBlock("dismiss", "dismiss", `{"name":"agent-0"}`), spawnBlock("replace", "new", "agent-0")), turn(textBlock("done")), turn(textBlock("done")))
	agentTurn(t, s, "reuse name and slot")
	if !results(s)[10].Outcome.OK {
		t.Fatal("dismiss did not release slot/name")
	}
}

func TestAgents_NamedHistoryAndBusyMessagesSurviveLaterTurn(t *testing.T) {
	s := agentFixture(t)
	var mu sync.Mutex
	var requests []ModelRequest
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			mu.Lock()
			requests = append(requests, req)
			mu.Unlock()
			return turn(textBlock("remembered result")), nil
		}}, nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("spawn", "first task", "tests"), callBlock("send", "send", `{"name":"tests","message":"queued task"}`)), turn(callBlock("wait", "wait", `{"ids":["tests"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "first user turn")
	s.model = NewScriptedModel(turn(callBlock("later", "send", `{"name":"tests","message":"later task"}`)), turn(callBlock("wait-later", "wait", `{"ids":["tests"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "ask my tests agent")
	mu.Lock()
	defer mu.Unlock()
	if len(requests) != 3 || len(requests[0].History) != 1 || len(requests[1].History) != 3 || len(requests[2].History) != 5 {
		t.Fatalf("history sizes: %+v", requests)
	}
	if requests[0].Scope.SessionID != requests[2].Scope.SessionID || s.agentRoster()[0].State != "idle" {
		t.Fatal("named Session lost")
	}
	if s.Turns() != 2 || len(agentResults(s.history)) != 3 {
		t.Fatal("results duplicated or counted as user turns")
	}
	for _, result := range results(s) {
		if result.Name == "wait" && strings.Contains(string(result.Outcome.Data), "remembered result") {
			t.Fatal("wait duplicated a queued-turn result")
		}
	}
}

func TestAgents_WaitTimeoutDoesNotCancelAndSubtreeCancellation(t *testing.T) {
	s := agentFixture(t)
	nested := make(chan struct{})
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
			if strings.HasPrefix(req.History[0].User.Text, "parent") {
				if req.Scope.Step == 1 {
					return turn(spawnBlock("nested", "leaf", "leaf")), nil
				}
			} else {
				close(nested)
			}
			<-ctx.Done()
			return ModelResponse{}, ctx.Err()
		}}, nil
	}
	step := 0
	s.model = agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
		step++
		switch step {
		case 1:
			return turn(spawnBlock("top", "parent", "parent")), nil
		case 2:
			select {
			case <-nested:
			case <-ctx.Done():
				t.Error("nested did not start")
			}
			return turn(callBlock("timeout", "wait", `{"ids":["parent"],"timeout":0}`)), nil
		case 3:
			var data struct {
				TimedOut bool `json:"timed_out"`
			}
			json.Unmarshal(results(s)[1].Outcome.Data, &data)
			if !data.TimedOut || len(s.runningAgents()) != 1 {
				t.Error("timeout cancelled or failed to report")
			}
			return turn(callBlock("cancel", "cancel", `{"name":"parent"}`)), nil
		default:
			return turn(textBlock("cancelled subtree")), nil
		}
	}}
	agentTurn(t, s, "cancel")
	ttree := s.agents
	ttree.mu.Lock()
	defer ttree.mu.Unlock()
	for _, n := range ttree.nodes {
		if n.working {
			t.Error("subtree still working")
		}
		if n.depth == 2 && !n.ended {
			t.Error("descendant did not end")
		}
	}
	if len(agentResults(s.history)) != 1 || agentResults(s.history)[0].Result.Status != StatusCancelled {
		t.Fatal("cancel result missing")
	}
}

func TestAgents_ResetBlockedAgent(t *testing.T) {
	s := agentFixture(t)
	s.agentModel = func(Config, *Trace) (Model, error) {
		return NewScriptedModel(turn(), turn(textBlock("recovered"))), nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("s", "invalid", "blocked")), turn(callBlock("w", "wait", `{"ids":["blocked"]}`)), turn(callBlock("deny", "send", `{"name":"blocked","message":"no"}`), callBlock("r", "reset", `{"name":"blocked"}`), callBlock("send", "send", `{"name":"blocked","message":"new task"}`)), turn(callBlock("w2", "wait", `{"ids":["blocked"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "reset")
	if results(s)[2].Outcome.OK || !results(s)[3].Outcome.OK || s.agentRoster()[0].State != "idle" {
		t.Fatalf("reset outcomes %+v", results(s))
	}
	all := agentResults(s.history)
	if len(all) != 2 || all[0].Result.Status != StatusProtocolError || all[1].Result.Reply != "recovered" {
		t.Fatalf("results %+v", all)
	}
	events := readEvents(t, s.agentRoster()[0].TracePath)
	starts := 0
	for _, e := range events {
		if e.Type == "run.started" {
			starts++
			if starts == 2 && e.Data.(map[string]any)["initial_history"] != nil {
				t.Fatal("reset retained history")
			}
		}
	}
	if starts != 2 {
		t.Fatal("agent trace did not retain both runs")
	}
}

func TestAgents_ReadOnlyPlanAndWorkspaceFixedAtSpawn(t *testing.T) {
	for _, plan := range []bool{false, true} {
		t.Run(fmt.Sprint(plan), func(t *testing.T) {
			s := agentFixture(t)
			s.planMode = plan
			if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, "live.txt"), []byte("live evidence"), 0600); err != nil {
				t.Fatal(err)
			}
			s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
				if !cfg.Registry.Mode().ReadOnly || cfg.PlanMode != plan {
					t.Error("mode widened")
				}
				return NewScriptedModel(turn(callBlock("write", "write_file", `{"path":"live.txt","content":"changed"}`), callBlock("exec", "exec", `{"argv":["touch","bad"],"cwd":"."}`), callBlock("read", "read_file", `{"path":"live.txt"}`)), turn(callBlock("switch", "switch_workspace", `{"path":"/"}`)), turn(textBlock("read-only result"))), nil
			}
			s.model = NewScriptedModel(turn(spawnBlock("s", "read", "reader")), turn(callBlock("w", "wait", `{"ids":["reader"]}`)), turn(textBlock("done")))
			agentTurn(t, s, "plan parent can delegate")
			path := s.agentRoster()[0].TracePath
			denied, read := 0, false
			for _, e := range readEvents(t, path) {
				if e.Type == "tool.finished" {
					d := e.Data.(map[string]any)
					o := d["outcome"].(map[string]any)
					if o["ok"] == false {
						denied++
					}
					if d["name"] == "read_file" && o["ok"] == true {
						read = true
					}
				}
			}
			if denied != 3 || !read {
				t.Fatalf("authority denied=%d read=%v", denied, read)
			}
			b, _ := os.ReadFile(filepath.Join(s.cfg.WorkspacePath, "live.txt"))
			if string(b) != "live evidence" {
				t.Fatal("agent wrote workspace")
			}
			s.planMode = !plan
			s.agents.mu.Lock()
			cfg := s.agents.nodes["t1"].session.cfg
			s.agents.mu.Unlock()
			if cfg.PlanMode != plan || cfg.WorkspacePath != s.cfg.WorkspacePath {
				t.Fatal("spawn ceiling changed")
			}
		})
	}
}

func TestAgents_RosterSurvivesCompactionModelSwitchAndResume(t *testing.T) {
	s := agentFixture(t)
	s.store = testSessionStore(t, s.ID)
	s.model = NewScriptedModel(turn(spawnBlock("s", "inspect tests", "tests")), turn(callBlock("w", "wait", `{"ids":["tests"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "keep named agent")
	for _, e := range s.history {
		if e.Assistant != nil {
			e.Assistant.Native = agentNativeResponse(*e.Assistant, openaiName).Native
		}
	}
	s.model = NewScriptedModel(turn(textBlock("compact handoff")))
	compact, _, err := s.Compact(context.Background(), "", NewID(), filepath.Join(t.TempDir(), "compact.jsonl"))
	if err != nil || compact.Status != StatusCompleted {
		t.Fatalf("compact %+v %v", compact, err)
	}
	c := &conversation{session: s, trace: s.trace, progress: io.Discard, client: NewHTTPClient(), keys: map[string]string{openaiName: "test"}}
	info, _ := findModel("gpt-6-luna")
	c.switchTo(info)
	fresh := c.session
	t.Cleanup(func() {
		if fresh.store != nil {
			fresh.store.close()
		}
	})
	fresh.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
		user := req.History[len(req.History)-1].User
		if user == nil || !strings.Contains(string(user.Roster), `"name":"tests"`) {
			t.Error("roster lost on fresh model switch")
		}
		return turn(textBlock("done")), nil
	}}
	agentTurn(t, fresh, "ask my test agent")
	cp, err := fresh.store.load(fresh.ID)
	if err != nil {
		t.Fatal(err)
	}
	resumed := NewSession(fresh.cfg, NewScriptedModel(turn(textBlock("ended"))), NewTrace(io.Discard), io.Discard)
	resumed.restore(cp)
	if len(resumed.agentRoster()) != 1 || resumed.agentRoster()[0].State != "ended at resume" {
		t.Fatalf("resume roster %+v", resumed.agentRoster())
	}
	if _, err := resumed.sendAgent(context.Background(), agentTargetArgs{ID: "t1"}, "must not restart"); err == nil {
		t.Fatal("resumed old agent")
	}
	agentTurn(t, resumed, "what happened to my agent?")
	last := resumed.history[len(resumed.history)-2].User
	if !strings.Contains(string(last.Roster), "ended at resume") {
		t.Fatal("resume tombstone not sent")
	}
}

func TestAgents_OptInAndDisabledRequestBytes(t *testing.T) {
	root := t.TempDir()
	var disabled, explicit bytes.Buffer
	for _, provider := range []string{openaiName, anthropicName} {
		for _, flag := range []string{"", "--agents=false", "--agents"} {
			var out, stderr bytes.Buffer
			args := []string{"run", "--workspace", root, "--provider", provider, "--show-context"}
			if flag != "" {
				args = append(args, flag)
			}
			args = append(args, "task")
			if code := Main(context.Background(), args, strings.NewReader(""), &out, &stderr); code != 0 {
				t.Fatalf("preview %d %s", code, stderr.String())
			}
			if strings.Contains(out.String(), `"name":"spawn"`) != (flag == "--agents") {
				t.Fatal("opt-in declaration")
			}
			if flag == "" {
				disabled = out
			}
			if flag == "--agents=false" {
				explicit = out
			}
		}
		if !bytes.Equal(disabled.Bytes(), explicit.Bytes()) {
			t.Fatal("disabled request bytes changed")
		}
	}
	var out, stderr bytes.Buffer
	if code := Main(context.Background(), []string{"run", "--child-runs", "task"}, strings.NewReader(""), &out, &stderr); code != exitUsage {
		t.Fatal("old flag still supported")
	}
}

func TestAgents_TokenAndStepCapsAndTreeCost(t *testing.T) {
	for _, tokens := range []bool{false, true} {
		t.Run(fmt.Sprint(tokens), func(t *testing.T) {
			s := agentFixture(t)
			s.agentModel = func(Config, *Trace) (Model, error) {
				return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					resp := turn(callBlock(fmt.Sprintf("read-%d", req.Scope.Step), "list_files", `{"path":"."}`))
					resp.Usage = Usage{Known: true, InputTokens: 1, OutputTokens: 1}
					if tokens {
						resp.Usage.InputTokens = maxAgentTokens
					}
					return resp, nil
				}}, nil
			}
			s.model = NewScriptedModel(turn(spawnBlock("s", "cap", "cap")), turn(callBlock("w", "wait", `{"ids":["cap"]}`)), turn(textBlock("done")))
			result := agentTurn(t, s, "caps")
			all := agentResults(s.history)
			if len(all) != 1 || all[0].Result.Status != StatusLimitExceeded || result.TreeCost == nil {
				t.Fatalf("caps %+v %+v", all, result)
			}
			if tokens && all[0].Result.Steps != 1 || !tokens && all[0].Result.Steps != maxAgentSteps {
				t.Fatal("cap not enforced")
			}
			if result.TreeCost.InputTokens != s.agentRoster()[0].Cost.InputTokens+result.Usage.InputTokens {
				t.Fatal("tree tokens double-counted")
			}
		})
	}
}

func agentNativeResponse(resp ModelResponse, provider string) ModelResponse {
	resp.Native.Provider = anthropicProvider
	if provider == openaiName {
		resp.Native.Provider = openaiProvider
	}
	for _, b := range resp.Blocks {
		var item any
		if provider == openaiName {
			if b.Call != nil {
				item = map[string]any{"type": "function_call", "call_id": b.Call.CallID, "name": b.Call.Name, "arguments": b.Call.Arguments}
			} else {
				item = map[string]any{"type": "message", "role": "assistant", "content": []any{map[string]any{"type": "output_text", "text": b.Text}}}
			}
		} else {
			if b.Call != nil {
				var args any
				json.Unmarshal([]byte(b.Call.Arguments), &args)
				item = map[string]any{"type": "tool_use", "id": b.Call.CallID, "name": b.Call.Name, "input": args}
			} else {
				item = map[string]any{"type": "text", "text": b.Text}
			}
		}
		raw, _ := json.Marshal(item)
		resp.Native.Items = append(resp.Native.Items, raw)
	}
	return resp
}

func TestAgents_EncodersKeepNativePairsAndCachePrefix(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		t.Run(provider, func(t *testing.T) {
			s := agentFixture(t)
			s.cfg.Provider = provider
			if provider == anthropicName {
				s.cfg.Model = "claude-sonnet-4-6"
			}
			s.planMode = true
			s.snapshot = func(context.Context, string, bool) json.RawMessage {
				return json.RawMessage(`{"kind":"workspace_state","date":"fixed"}`)
			}
			if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, "file.txt"), []byte("live-file-canary"), 0600); err != nil {
				t.Fatal(err)
			}
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "parent-history-canary"}}}
			s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
				return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					body, err := encodeRequest(cfg, req)
					if err != nil {
						t.Error(err)
					}
					if bytes.Contains(body, []byte("parent-history-canary")) {
						t.Error("parent history shared")
					}
					if req.Scope.Step == 1 {
						return agentNativeResponse(turn(callBlock("read", "read_file", `{"path":"file.txt"}`)), provider), nil
					}
					if !bytes.Contains(body, []byte("live-file-canary")) {
						t.Error("agent did not read live bytes")
					}
					return agentNativeResponse(turn(textBlock("result-canary")), provider), nil
				}}, nil
			}
			initialInstructions := instructions(s.cfg)
			step := 0
			s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
				step++
				body, err := encodeRequest(s.cfg, req)
				if err != nil {
					t.Error(err)
				}
				if req.Instructions != initialInstructions {
					t.Error("result changed cached instructions")
				}
				if step == 1 {
					if !bytes.Contains(body, []byte("workspace_state")) || !bytes.Contains(body, []byte("agent_roster")) {
						t.Error("sibling user snapshot parts missing")
					}
					return agentNativeResponse(turn(spawnBlock("spawn", "read live evidence", "reader")), provider), nil
				}
				if step == 2 {
					return agentNativeResponse(turn(callBlock("wait", "wait", `{"ids":["reader"]}`)), provider), nil
				}
				if !bytes.Contains(body, []byte("result-canary")) {
					t.Error("delivered result not encoded")
				}
				return agentNativeResponse(turn(textBlock("done")), provider), nil
			}}
			agentTurn(t, s, "parent user task")
			s.planMode = false
			if planMarkerFor(s.history, false) != "ended" {
				t.Error("agent result erased last human plan marker")
			}
			handoff, err := renderModelHandoff(s.history)
			if err != nil || !strings.Contains(handoff.Text, "untrusted_agent_data") || handoff.Plan != "on" {
				t.Fatalf("model-switch view %+v %v", handoff, err)
			}
		})
	}
}

func TestAgents_ToolArgumentsAndDuplicateName(t *testing.T) {
	cases := []struct{ name, args string }{
		{"spawn", `{"task":"task","context":{},"name":"Bad"}`},
		{"spawn", `{"task":"task","context":{},"name":"bad--slug"}`},
		{"spawn", `{"task":"task","context":{},"name":"-bad"}`},
		{"spawn", `{"task":"task","context":null}`},
		{"spawn", `{"task":"task","context":{"unknown":true}}`},
		{"spawn", `{"task":"task","context":{"files":["../outside"]}}`},
		{"send", `{"id":"t1","name":"named","message":"task"}`},
		{"wait", `{"ids":[],"timeout":1}`},
		{"wait", `{"ids":["unknown"],"timeout":0}`},
		{"wait", `{"ids":["t1"],"timeout":null}`},
		{"wait", `{"ids":["t1"],"timeout":601}`},
		{"threads", `{"extra":true}`},
		{"cancel", `{}`},
		{"reset", `{"id":"t1","extra":true}`},
	}
	for _, tc := range cases {
		t.Run(tc.name+tc.args, func(t *testing.T) {
			s := agentFixture(t)
			s.model = NewScriptedModel(turn(callBlock("invalid", tc.name, tc.args)), turn(textBlock("done")))
			agentTurn(t, s, "invalid args")
			if results(s)[0].Outcome.OK || len(s.agentRoster()) != 0 {
				t.Fatal("invalid arguments admitted work")
			}
		})
	}
	s := agentFixture(t)
	s.model = NewScriptedModel(turn(spawnBlock("a", "task", "named"), spawnBlock("b", "task", "named")), turn(callBlock("w", "wait", `{"ids":["named"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "unique names")
	if results(s)[1].Outcome.OK || !strings.Contains(results(s)[1].Outcome.Message, "already live") {
		t.Fatal("duplicate name admitted")
	}
}

func TestAgents_AbnormalRootExitCancelsAndJoins(t *testing.T) {
	s := agentFixture(t)
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(ctx context.Context, _ ModelRequest) (ModelResponse, error) {
			<-ctx.Done()
			return ModelResponse{}, ctx.Err()
		}}, nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("s", "waiting", "worker")), turn())
	result, err := s.Turn(context.Background(), "abnormal", NewID(), filepath.Join(t.TempDir(), "trace.jsonl"))
	if err != nil || result.Status != StatusProtocolError || len(s.runningAgents()) != 0 {
		t.Fatalf("abnormal %+v %v", result, err)
	}
	if len(agentResults(s.history)) != 1 {
		t.Fatal("failed root lost child terminal result")
	}
}

func TestAgents_ResetInSpawnBatchDoesNotEraseUndeliveredResult(t *testing.T) {
	s := agentFixture(t)
	s.agentModel = func(Config, *Trace) (Model, error) {
		return NewScriptedModel(turn(), turn(textBlock("recovered"))), nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("s", "invalid", "named"), callBlock("w", "wait", `{"ids":["named"]}`), callBlock("r", "reset", `{"name":"named"}`), callBlock("send", "send", `{"name":"named","message":"try new work"}`)), turn(callBlock("w2", "wait", `{"ids":["named"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "reset in same batch")
	all := agentResults(s.history)
	if len(all) != 2 || all[0].Result.Status != StatusProtocolError || all[1].Result.Reply != "recovered" {
		t.Fatalf("reset erased an immutable completion: %+v", all)
	}
}

func TestAgents_NormalModelSwitchRetainsNamedAgentAndFreshRoster(t *testing.T) {
	s := agentFixture(t)
	s.model = NewScriptedModel(agentNativeResponse(turn(spawnBlock("s", "task", "named")), openaiName), agentNativeResponse(turn(callBlock("w", "wait", `{"ids":["named"]}`)), openaiName), agentNativeResponse(turn(textBlock("done")), openaiName))
	agentTurn(t, s, "first turn")
	c := &conversation{session: s, trace: s.trace, progress: io.Discard, client: NewHTTPClient(), keys: map[string]string{anthropicName: "test"}, traceDir: t.TempDir()}
	info, _ := findModel("claude-sonnet-5-5")
	handoff, err := c.transitionModel(context.Background(), info, false)
	if err != nil || handoff == nil {
		t.Fatalf("switch %+v %v", handoff, err)
	}
	s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
		body, err := EncodeAnthropicRequest(req)
		if err != nil {
			t.Error(err)
		}
		if !bytes.Contains(body, []byte("Agent roster when this message was sent")) || !bytes.Contains(body, []byte(`\"name\":\"named\"`)) {
			t.Error("fresh roster missing after normal switch")
		}
		return agentNativeResponse(turn(textBlock("continued")), anthropicName), nil
	}}
	result := agentTurn(t, s, "ask my named agent")
	if result.Reply != "continued" || len(s.agentRoster()) != 1 {
		t.Fatal("switch ended named agent")
	}
}

func TestAgents_ExplicitNameIsNotConfusedWithAnotherThreadID(t *testing.T) {
	s := agentFixture(t)
	s.model = NewScriptedModel(turn(spawnBlock("a", "first task", "first"), spawnBlock("b", "second task", "t1"), callBlock("send", "send", `{"name":"t1","message":"named target"}`)), turn(callBlock("wait", "wait", `{"ids":["t1","t2"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "ID and slug collision")
	roster := s.agentRoster()
	if len(roster) != 2 || roster[0].Task != "first task" || roster[1].Task != "named target" {
		t.Fatalf("wrong child addressed: %+v", roster)
	}
}

func TestAgents_ReadOnlyParentCannotGrantMissingReadTools(t *testing.T) {
	s := agentFixture(t)
	registry, err := NewRegistry(Mode{ReadOnly: true}, append(NewAgentTools(), NewListFilesTool(s.cfg.Workspace))...)
	if err != nil {
		t.Fatal(err)
	}
	s.cfg.Registry = registry.bindWorkspace(s, s.workspace.active)
	s.planMode = true
	s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
		if cfg.Registry.Mode().String() != "read only" || !cfg.PlanMode {
			t.Error("lost inherited mode")
		}
		if _, found := cfg.Registry.Lookup("read_file"); found {
			t.Error("spawn widened parent's read capabilities")
		}
		return NewScriptedModel(turn(callBlock("read", "read_file", `{"path":"x"}`), callBlock("list", "list_files", `{"path":"."}`)), turn(textBlock("done"))), nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("s", "read", "named")), turn(callBlock("w", "wait", `{"ids":["named"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "read-only plan parent")
}

func TestAgents_ModelOverrideIsFixedInNamedSession(t *testing.T) {
	s := agentFixture(t)
	var selected Config
	s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
		selected = cfg
		return NewScriptedModel(turn(textBlock("done")), turn(textBlock("later"))), nil
	}
	s.model = NewScriptedModel(turn(callBlock("s", "spawn", `{"task":"inspect","context":{"brief":"fresh"},"name":"named","model":"claude-sonnet-5-5"}`)), turn(callBlock("w", "wait", `{"ids":["named"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "override")
	if selected.Provider != anthropicName || selected.Model != "claude-sonnet-5-5" {
		t.Fatal("model override ignored")
	}
	s.cfg.Model = "gpt-6-luna"
	s.model = NewScriptedModel(turn(callBlock("send", "send", `{"name":"named","message":"later"}`)), turn(callBlock("w2", "wait", `{"ids":["named"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "later turn")
	s.agents.mu.Lock()
	defer s.agents.mu.Unlock()
	if s.agents.nodes["t1"].session.cfg.Model != "claude-sonnet-5-5" {
		t.Fatal("root switch changed named model")
	}
}

func TestAgents_WaitZeroOnIdleAgentIsNotTimeout(t *testing.T) {
	s := agentFixture(t)
	s.model = NewScriptedModel(turn(spawnBlock("s", "task", "named"), callBlock("w", "wait", `{"ids":["named"]}`), callBlock("zero", "wait", `{"ids":["named"],"timeout":0}`)), turn(textBlock("done")))
	agentTurn(t, s, "already idle")
	var data struct {
		TimedOut bool `json:"timed_out"`
	}
	if err := json.Unmarshal(results(s)[2].Outcome.Data, &data); err != nil || data.TimedOut {
		t.Fatalf("idle wait reported timeout: %+v %v", data, err)
	}
}

func TestAgents_ContextPathsReadLiveBytesWithoutParentHistory(t *testing.T) {
	s := agentFixture(t)
	path := filepath.Join(s.cfg.WorkspacePath, "source.txt")
	if err := os.WriteFile(path, []byte("before spawn"), 0600); err != nil {
		t.Fatal(err)
	}
	release := make(chan struct{})
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
			if req.Scope.Step == 1 {
				if len(req.History) != 1 || !strings.Contains(req.History[0].User.Text, `"files":["source.txt"]`) {
					t.Error("context paths not sent independently")
				}
				select {
				case <-release:
				case <-ctx.Done():
					return ModelResponse{}, ctx.Err()
				}
				return turn(callBlock("read", "read_file", `{"path":"source.txt"}`)), nil
			}
			found := false
			for _, e := range req.History {
				if e.Tool != nil && bytes.Contains(e.Tool.Outcome.Data, []byte("after spawn")) {
					found = true
				}
			}
			if !found {
				t.Error("agent saw a frozen snapshot, not live bytes")
			}
			return turn(textBlock("live result")), nil
		}}, nil
	}
	step := 0
	s.model = agentTestModel{generate: func(_ context.Context, _ ModelRequest) (ModelResponse, error) {
		step++
		switch step {
		case 1:
			return turn(callBlock("s", "spawn", `{"task":"inspect","context":{"brief":"read live source","files":["source.txt"]}}`)), nil
		case 2:
			if err := os.WriteFile(path, []byte("after spawn"), 0600); err != nil {
				t.Error(err)
			}
			close(release)
			return turn(callBlock("w", "wait", `{"ids":["t1"]}`)), nil
		default:
			return turn(textBlock("done")), nil
		}
	}}
	agentTurn(t, s, "parent brief is not shared")
}

func TestAgents_WorkspaceSnapshotCannotExecuteCleanFilters(t *testing.T) {
	s := agentFixture(t)
	root := s.cfg.WorkspacePath
	workspaceGit(t, root, "init", "--template="+t.TempDir(), "-b", "main")
	script := filepath.Join(t.TempDir(), "clean.sh")
	marker := script + ".marker"
	if err := os.WriteFile(script, []byte("#!/bin/sh\n: > \"$0.marker\"\ncat\n"), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, ".gitattributes"), []byte("f.txt filter=agent-test\n"), 0600); err != nil {
		t.Fatal(err)
	}
	file := filepath.Join(root, "f.txt")
	if err := os.WriteFile(file, []byte("data\n"), 0600); err != nil {
		t.Fatal(err)
	}
	workspaceGit(t, root, "config", "filter.agent-test.clean", script)
	workspaceGit(t, root, "add", ".")
	workspaceGit(t, root, "commit", "-m", "fixture")
	workspaceGit(t, root, "remote", "add", "origin", ".")
	workspaceGit(t, root, "update-ref", "refs/remotes/origin/main", "HEAD")
	workspaceGit(t, root, "branch", "--set-upstream-to=origin/main", "main")
	changed := time.Now().Add(2 * time.Second)
	if err := os.Chtimes(file, changed, changed); err != nil {
		t.Fatal(err)
	}
	collectSnapshot(context.Background(), root, false)
	if _, err := os.Stat(marker); err != nil {
		t.Fatal("fixture did not execute clean filter")
	}
	if err := os.Remove(marker); err != nil {
		t.Fatal(err)
	}
	changed = changed.Add(time.Second)
	if err := os.Chtimes(file, changed, changed); err != nil {
		t.Fatal(err)
	}
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			assertRefOnlySnapshot(t, req.History[0].User.Workspace, "main")
			return turn(textBlock("safe snapshot")), nil
		}}, nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("s", "inspect", "named")), turn(callBlock("w", "wait", `{"ids":["named"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "no commands in metadata collection")
	if _, err := os.Stat(marker); !os.IsNotExist(err) {
		t.Fatal("agent metadata executed a configured clean filter")
	}
}

func TestAgents_CachedGrowingContextReachesStepCap(t *testing.T) {
	s := agentFixture(t)
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			resp := turn(callBlock(fmt.Sprintf("read-%d", req.Scope.Step), "list_files", `{"path":"."}`))
			input := int64(6000 + 4000*req.Scope.Step)
			resp.Usage = Usage{Known: true, InputTokens: input, CachedInputTokens: input - 4000, OutputTokens: 100}
			return resp, nil
		}}, nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("s", "growing context", "cached"), callBlock("w", "wait", `{"ids":["cached"]}`)), turn(textBlock("done")))
	result := agentTurn(t, s, "cached reads")
	child := agentResults(s.history)[0].Result
	if child.Steps != maxAgentSteps || child.Status != StatusLimitExceeded || child.Usage.InputTokens <= maxAgentTokens || child.Usage.CachedInputTokens == 0 || result.TreeCost.InputTokens != child.Usage.InputTokens+result.Usage.InputTokens {
		t.Fatalf("cached usage/cap: %+v %+v", child, result)
	}
	if agentTokenLimit(Usage{InputTokens: 1, CachedInputTokens: 9, OutputTokens: maxAgentTokens}) == false {
		t.Fatal("anomalous cache count subtracted output usage")
	}
}

func TestAgents_WaitAfterOtherWorkUsesReceiptAndDoesNotDuplicateReply(t *testing.T) {
	s := agentFixture(t)
	step := 0
	s.model = agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
		step++
		switch step {
		case 1:
			return turn(spawnBlock("s", "inspect", "")), nil
		case 2:
			// Child completes while the parent's request is doing unrelated work.
			if _, err := s.waitAgents(ctx, []string{"t1"}); err != nil {
				t.Error(err)
			}
			return turn(callBlock("other", "list_files", `{"path":"."}`)), nil
		case 3:
			if len(agentResults(req.History)) != 1 || len(s.agentRoster()) != 0 {
				t.Error("anonymous delivery/cleanup missing")
			}
			return turn(callBlock("later", "wait", `{"ids":["t1"],"timeout":0}`)), nil
		default:
			return turn(textBlock("done")), nil
		}
	}}
	agentTurn(t, s, "other work")
	out := results(s)[2].Outcome
	var data struct {
		Statuses []agentWaitStatus `json:"statuses"`
		TimedOut bool              `json:"timed_out"`
	}
	if err := json.Unmarshal(out.Data, &data); err != nil || !out.OK || data.TimedOut || len(data.Statuses) != 1 || data.Statuses[0].DeliveryStep == nil || *data.Statuses[0].DeliveryStep != 2 || data.Statuses[0].DeliveryRunID == "" || !strings.Contains(data.Statuses[0].Message, "already delivered at step 2") || strings.Contains(string(out.Data), "child done") || len(agentResults(s.history)) != 1 {
		t.Fatalf("receipt/outcome: %+v %+v", out, data)
	}
	if _, err := s.waitAgents(context.Background(), []string{"t999"}); err == nil || !strings.Contains(err.Error(), `ID or name "t999"`) {
		t.Fatalf("misleading unknown target: %v", err)
	}
	s.agents.mu.Lock()
	child := NewSession(s.cfg, s.model, NewTrace(io.Discard), io.Discard)
	child.agents, child.agentID = s.agents, "other-parent"
	s.agents.mu.Unlock()
	if _, err := child.waitAgents(context.Background(), []string{"t1"}); err == nil {
		t.Fatal("foreign receipt visible")
	}
}

func TestAgents_WaitAndBoundaryBoundEscapedReply(t *testing.T) {
	s := agentFixture(t)
	reply := strings.Repeat("雪\x00\"\\<", 20000)
	s.agentModel = func(Config, *Trace) (Model, error) { return NewScriptedModel(turn(textBlock(reply))), nil }
	s.model = NewScriptedModel(turn(spawnBlock("s", "large", "large"), callBlock("w", "wait", `{"ids":["large"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "oversized result")
	if strings.Contains(string(results(s)[1].Outcome.Data), "雪") || strings.Contains(string(results(s)[1].Outcome.Data), `"results"`) {
		t.Fatal("wait included content")
	}
	all := agentResults(s.history)
	if len(all) != 1 || !all[0].Truncated || all[0].Result.TracePath == "" || all[0].Result.Reply == reply {
		t.Fatalf("unbounded result: %+v", all)
	}
	for _, e := range s.history {
		if e.User != nil && e.User.Source == "agent" {
			raw, _ := json.Marshal(e.User)
			if len(raw) > MaxResultBytes || !json.Valid(raw) {
				t.Fatalf("encoded entry bytes=%d", len(raw))
			}
		}
	}
	found := false
	for _, e := range readEvents(t, all[0].Result.TracePath) {
		if e.Type == "run.finished" && e.Data.(map[string]any)["reply"] == reply {
			found = true
		}
	}
	if !found {
		t.Fatal("full reply missing from lifetime trace")
	}
}

func TestAgents_ReceiptsAreBoundedAndReleaseCapacity(t *testing.T) {
	s := agentFixture(t)
	for i := 0; i < maxAgentReceipts+2; i++ {
		s.model = NewScriptedModel(turn(spawnBlock(fmt.Sprintf("s%d", i), "one", ""), callBlock(fmt.Sprintf("w%d", i), "wait", fmt.Sprintf(`{"ids":["t%d"]}`, i+1))), turn(textBlock("done")))
		agentTurn(t, s, "collect")
	}
	if len(s.agentReceipts) != maxAgentReceipts || len(s.agentRoster()) != 0 {
		t.Fatal("unbounded receipts or retained capacity")
	}
	if _, err := s.waitAgents(context.Background(), []string{"t1"}); err == nil {
		t.Fatal("old receipt not evicted")
	}
	if _, err := s.waitAgents(context.Background(), []string{"t66"}); err != nil {
		t.Fatal(err)
	}
}

func TestAgents_ResumedTombstonesDismissByIDAndName(t *testing.T) {
	for _, target := range []agentTargetArgs{{ID: "t1"}, {Name: "tests"}} {
		t.Run(fmt.Sprint(target), func(t *testing.T) {
			s := agentFixture(t)
			s.restore(chatCheckpoint{ID: s.ID, Agents: true, AgentRoster: []AgentThread{{ID: "t1", ParentID: "root", Name: "tests"}}})
			s.store = testSessionStore(t, s.ID)
			if _, err := s.sendAgent(context.Background(), target, "no restart"); err == nil {
				t.Fatal("restarted tombstone")
			}
			if err := s.controlAgent(context.Background(), target, "reset"); err == nil {
				t.Fatal("reset tombstone")
			}
			if err := s.controlAgent(context.Background(), target, "dismiss"); err != nil {
				t.Fatal(err)
			}
			if len(s.agentRoster()) != 0 {
				t.Fatal("dismiss retained roster observation")
			}
			s.model = NewScriptedModel(turn(textBlock("done")))
			agentTurn(t, s, "next message")
			cp, err := s.store.load(s.ID)
			if err != nil || len(cp.AgentRoster) != 0 {
				t.Fatalf("dismiss not persisted: %+v %v", cp.AgentRoster, err)
			}
			if strings.Contains(string(s.history[0].User.Roster), "tests") {
				t.Fatal("next message retained tombstone")
			}
			s.model = NewScriptedModel(turn(spawnBlock("s", "replacement", "tests"), callBlock("w", "wait", `{"ids":["tests"]}`)), turn(textBlock("done")))
			agentTurn(t, s, "reuse name")
			if s.agentRoster()[0].ID != "t2" {
				t.Fatal("ID reused")
			}
		})
	}
}

func TestAgents_CustomTraceDirectorySurvivesNestingAndFreshSwitch(t *testing.T) {
	s := agentFixture(t)
	s.agentTraceDir = t.TempDir()
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			if req.Scope.Step == 1 && strings.HasPrefix(req.History[0].User.Text, "parent") {
				return turn(spawnBlock("nested", "leaf", "leaf"), callBlock("w", "wait", `{"ids":["leaf"]}`)), nil
			}
			return turn(textBlock("done")), nil
		}}, nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("s", "parent", "parent"), callBlock("w", "wait", `{"ids":["parent"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "nested")
	c := &conversation{session: s, trace: s.trace, progress: io.Discard, client: NewHTTPClient(), keys: map[string]string{openaiName: "test"}}
	info, _ := findModel("gpt-6-luna")
	c.switchTo(info)
	statuses, err := c.session.waitAgents(context.Background(), []string{"parent"})
	if err != nil || len(statuses) != 1 || statuses[0].DeliveryRunID == "" {
		t.Fatalf("fresh switch lost delivery receipt: %+v %v", statuses, err)
	}
	c.session.model = NewScriptedModel(turn(spawnBlock("new", "new agent", "new"), callBlock("w", "wait", `{"ids":["new"]}`)), turn(textBlock("done")))
	agentTurn(t, c.session, "after fresh switch")
	s.agents.mu.Lock()
	defer s.agents.mu.Unlock()
	if len(s.agents.nodes) != 3 {
		t.Fatal("missing nested or fresh agent")
	}
	for _, n := range s.agents.nodes {
		if filepath.Dir(filepath.Dir(n.TracePath)) != s.agentTraceDir {
			t.Fatalf("trace outside override: %s", n.TracePath)
		}
		if _, err := os.Stat(n.TracePath); err != nil {
			t.Fatal(err)
		}
	}
}

func TestAgents_LiveNameWinsOverResumedTombstone(t *testing.T) {
	s := agentFixture(t)
	s.restore(chatCheckpoint{ID: s.ID, Agents: true, AgentRoster: []AgentThread{{ID: "t1", ParentID: "root", Name: "tests"}}})
	s.model = NewScriptedModel(turn(spawnBlock("s", "replacement", "tests"), callBlock("w", "wait", `{"ids":["tests"]}`), callBlock("dismiss-old", "dismiss", `{"id":"t1"}`)), turn(textBlock("done")))
	agentTurn(t, s, "reuse ended name")
	if !results(s)[1].Outcome.OK || !results(s)[2].Outcome.OK || len(s.agentRoster()) != 1 || s.agentRoster()[0].ID != "t2" {
		t.Fatalf("live name/tombstone targeting: %+v %+v", results(s), s.agentRoster())
	}
}
