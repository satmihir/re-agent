package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"testing"
)

func TestAgents_KindWireContract(t *testing.T) {
	for _, tc := range []struct {
		name, field, want string
	}{
		{"omitted", "", "task"},
		{"task", `"kind":"task",`, "task"},
		{"teammate", `"kind":"teammate",`, "teammate"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := agentFixture(t)
			s.model = NewScriptedModel(turn(callBlock("spawn", "spawn", `{`+tc.field+`"task":"inspect","name":"worker","context":{}}`), callBlock("wait", "wait", `{"ids":["worker"]}`)), turn(textBlock("done")))
			agentTurn(t, s, "kind")
			var spawned AgentThread
			if err := json.Unmarshal(results(s)[0].Outcome.Data, &spawned); err != nil || !results(s)[0].Outcome.OK || spawned.Kind != tc.want || spawned.Authority != "read" {
				t.Fatalf("spawned: %+v, %v", spawned, err)
			}
			tree := s.agentSubtree()
			if tree.TotalAgents != 1 || tree.Threads[0].Kind != tc.want {
				t.Fatalf("kind lost in accounting: %+v", tree)
			}
			if retained := len(s.agentRoster()) == 1; retained != (tc.want == "teammate") {
				t.Fatalf("wrong lifecycle for %s: %+v", tc.want, s.agentRoster())
			}
		})
	}
	for _, field := range []string{`"kind":null,`, `"kind":"",`, `"kind":"worker",`, `"kind":"Task",`, `"kind":" task",`, `"kind":false,`, `"kind":{},`} {
		t.Run(field, func(t *testing.T) {
			s := agentFixture(t)
			s.model = NewScriptedModel(turn(callBlock("spawn", "spawn", `{`+field+`"task":"inspect","name":"worker","context":{}}`)), turn(textBlock("done")))
			agentTurn(t, s, "invalid kind")
			if out := results(s)[0].Outcome; out.OK || out.Code != "invalid_arguments" || s.agentSubtree().TotalAgents != 0 {
				t.Fatalf("invalid kind admitted: %+v", out)
			}
		})
	}
	s := agentFixture(t)
	s.model = NewScriptedModel(turn(callBlock("spawn", "spawn", `{"kind":"teammate","task":"inspect","context":{}}`)), turn(textBlock("done")))
	agentTurn(t, s, "unnamed teammate")
	if results(s)[0].Outcome.OK || s.agentSubtree().TotalAgents != 0 {
		t.Fatal("unnamed teammate admitted")
	}
}

func TestAgents_OnlyRootCanSpawnTeammates(t *testing.T) {
	for _, kind := range []string{"task", "teammate"} {
		t.Run(kind, func(t *testing.T) {
			s := agentFixture(t)
			s.agentModel = func(Config, *Trace) (Model, error) {
				return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					if strings.HasPrefix(req.History[0].User.Text, "parent") && req.Scope.Step == 1 {
						return turn(teammateBlock("denied", "not allowed", "teammate"), spawnBlock("allowed", "leaf", "leaf"), callBlock("wait", "wait", `{"ids":["leaf"]}`)), nil
					}
					return turn(textBlock("done")), nil
				}}, nil
			}
			s.model = NewScriptedModel(turn(callBlock("spawn", "spawn", `{"kind":"`+kind+`","task":"parent","name":"parent","context":{}}`), callBlock("wait", "wait", `{"ids":["parent"]}`)), turn(textBlock("done")))
			agentTurn(t, s, "root only")
			tree := s.agentSubtree()
			if tree.TotalAgents != 2 || tree.Threads[1].Kind != "task" || tree.Threads[1].Depth != 2 {
				t.Fatalf("nested admission: %+v", tree)
			}
			found := false
			for _, event := range readEvents(t, tree.Threads[0].TracePath) {
				if event.Type == "tool.finished" {
					data := event.Data.(map[string]any)
					if data["call_id"] == "denied" {
						found = true
						if data["outcome"].(map[string]any)["ok"] != false {
							t.Error("child spawned a teammate")
						}
					}
				}
			}
			if !found {
				t.Fatal("missing teammate denial")
			}
		})
	}
}

func TestAgents_NamedTaskDrainsFollowupsThenReleasesName(t *testing.T) {
	s := agentFixture(t)
	s.agentModel = func(Config, *Trace) (Model, error) {
		return NewScriptedModel(turn(textBlock("first result")), turn(textBlock("followup result"))), nil
	}
	s.model = NewScriptedModel(
		turn(spawnBlock("spawn", "inspect", "worker"), callBlock("wait-first", "wait", `{"ids":["worker"]}`), callBlock("send-before-delivery", "send", `{"name":"worker","message":"followup"}`), callBlock("wait-followup", "wait", `{"ids":["worker"]}`)),
		turn(callBlock("send-after-delivery", "send", `{"name":"worker","message":"too late"}`), callBlock("wait-receipt", "wait", `{"ids":["t1"]}`), spawnBlock("replacement", "replacement", "worker"), callBlock("wait-new", "wait", `{"ids":["worker"]}`)),
		turn(textBlock("done")),
	)
	agentTurn(t, s, "task followups")
	got := results(s)
	if !got[2].Outcome.OK || got[4].Outcome.OK || !got[5].Outcome.OK || !got[6].Outcome.OK || len(s.agentRoster()) != 0 {
		t.Fatalf("task lifetime: %+v, %+v", got, s.agentRoster())
	}
	all := agentResults(s.history)
	if len(all) != 3 || all[0].ID != "t1" || all[1].ID != "t1" || all[2].ID != "t2" || all[1].Result.Reply != "followup result" {
		t.Fatalf("queued work or name reuse lost: %+v", all)
	}
}

func TestAgents_TeammateExceedsTaskCapsAndKeepsHistory(t *testing.T) {
	s := agentFixture(t)
	var selected Config
	s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
		selected = cfg
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			if req.MaxOutputTokens != 0 {
				t.Error("teammate requested task cap summary")
			}
			last := req.History[len(req.History)-1]
			if req.Scope.Step > 65 || last.User != nil && strings.HasPrefix(last.User.Text, "followup") {
				return turn(textBlock("completed")), nil
			}
			var calls []OutputBlock
			for i := 0; i < 4; i++ {
				calls = append(calls, callBlock(fmt.Sprintf("list-%d-%d", req.Scope.Step, i), "list_files", `{"path":"."}`))
			}
			resp := turn(calls...)
			resp.Usage = Usage{Known: true, InputTokens: 400001}
			return resp, nil
		}}, nil
	}
	s.model = NewScriptedModel(turn(teammateBlock("spawn", "work", "worker"), callBlock("wait", "wait", `{"ids":["worker"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "unbounded teammate")
	result := agentResults(s.history)[0].Result
	if selected.MaxSteps != 0 || selected.MaxToolCalls != 0 || result.Status != StatusCompleted || result.Steps != 66 || result.ToolCalls != 260 || result.Usage.InputTokens <= 400000 {
		t.Fatalf("teammate inherited task caps: config=%+v result=%+v", selected, result)
	}
	firstSession := s.agents.nodes["t1"].session.ID
	s.model = NewScriptedModel(turn(callBlock("send", "send", `{"name":"worker","message":"followup"}`), callBlock("wait-followup", "wait", `{"ids":["worker"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "next turn")
	if all := agentResults(s.history); len(all) != 2 || all[1].Result.Status != StatusCompleted || s.agents.nodes["t1"].session.ID != firstSession || len(s.agentRoster()) != 1 {
		t.Fatalf("teammate followup lost: %+v", all)
	}
}

func TestAgents_WaitAcceptsSixteenChildrenAndRejectsSeventeen(t *testing.T) {
	s := agentFixture(t)
	var batch []OutputBlock
	var ids []string
	for i := 0; i < 16; i++ {
		name := fmt.Sprintf("worker-%d", i)
		batch = append(batch, teammateBlock(fmt.Sprintf("spawn-%d", i), "inspect", name))
		ids = append(ids, name)
	}
	raw, _ := json.Marshal(map[string]any{"ids": ids})
	batch = append(batch, callBlock("wait-16", "wait", string(raw)))
	ids = append(ids, "extra")
	raw, _ = json.Marshal(map[string]any{"ids": ids})
	s.model = NewScriptedModel(turn(batch...), turn(callBlock("wait-17", "wait", string(raw))), turn(textBlock("done")))
	agentTurn(t, s, "wait boundary")
	got := results(s)
	if !got[16].Outcome.OK || got[17].Outcome.OK || got[17].Outcome.Code != "invalid_arguments" || len(s.agentRoster()) != 16 {
		t.Fatalf("wait boundary: %+v", got)
	}
	var waited struct {
		Statuses []agentWaitStatus `json:"statuses"`
	}
	if err := json.Unmarshal(got[16].Outcome.Data, &waited); err != nil || len(waited.Statuses) != 16 {
		t.Fatalf("wait omitted statuses: %+v %v", waited, err)
	}
}

func TestAgents_ResetPreservesKindAndAuthority(t *testing.T) {
	for _, kind := range []string{"task", "teammate"} {
		for _, authority := range []string{"read", "write"} {
			t.Run(kind+"/"+authority, func(t *testing.T) {
				s := agentFixture(t)
				s.agentModel = func(Config, *Trace) (Model, error) {
					return NewScriptedModel(turn(), turn(textBlock("recovered"))), nil
				}
				s.model = NewScriptedModel(turn(
					callBlock("spawn", "spawn", `{"kind":"`+kind+`","authority":"`+authority+`","task":"inspect","name":"worker","context":{}}`),
					callBlock("wait-blocked", "wait", `{"ids":["worker"]}`),
					callBlock("reset", "reset", `{"name":"worker"}`),
					callBlock("send", "send", `{"name":"worker","message":"recover"}`),
					callBlock("wait-recovered", "wait", `{"ids":["worker"]}`)), turn(textBlock("done")))
				agentTurn(t, s, "reset")
				got := results(s)
				if !got[2].Outcome.OK || !got[3].Outcome.OK {
					t.Fatalf("reset failed: %+v", got)
				}
				reset := s.agentSubtree().Threads[0]
				if reset.Kind != kind || reset.Authority != authority {
					t.Fatalf("reset changed identity: %+v", reset)
				}
				all := agentResults(s.history)
				if len(all) != 2 || all[0].Result.Status != StatusProtocolError || all[1].Result.Reply != "recovered" || s.agentSubtree().Threads[0].Kind != kind {
					t.Fatalf("reset changed lifecycle: %+v", all)
				}
				if kind == "teammate" {
					n := s.agents.nodes["t1"]
					if n.session.cfg.agentKind != kind || n.session.cfg.Registry.Mode().ReadOnly != (authority == "read") || n.session.cfg.MaxSteps != 0 || n.session.cfg.MaxToolCalls != 0 {
						t.Fatalf("reset changed config: %+v", n.session.cfg)
					}
				}
			})
		}
	}
}

func TestAgents_KindCheckpointAndLegacyDefaults(t *testing.T) {
	s := agentFixture(t)
	s.store = testSessionStore(t, s.ID)
	s.model = NewScriptedModel(turn(teammateBlock("teammate", "ongoing", "partner"), spawnBlock("task", "bounded", "named-task"), callBlock("wait", "wait", `{"ids":["partner","named-task"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "persist kinds")
	cp, err := s.store.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(cp.AgentRoster) != 1 || cp.AgentRoster[0].Kind != "teammate" || cp.AgentSubtree == nil || len(cp.AgentSubtree.Threads) != 2 || cp.AgentSubtree.Threads[1].Kind != "task" {
		t.Fatalf("checkpoint lost kinds: roster=%+v subtree=%+v", cp.AgentRoster, cp.AgentSubtree)
	}
	cp.AgentRoster = append(cp.AgentRoster, AgentThread{ID: "t3", ParentID: "root", Name: "legacy", State: "idle", Authority: "read"})
	cp.AgentSubtree.Threads = append(cp.AgentSubtree.Threads, agentObservation{AgentThread: AgentThread{ID: "t3", ParentID: "root", Name: "legacy", State: "idle", Authority: "read"}, Depth: 1}, agentObservation{AgentThread: AgentThread{ID: "t4", ParentID: "root", State: "delivered"}, Depth: 1})
	cp.AgentSubtree.TotalAgents = 4
	restored := agentFixture(t)
	restored.restore(cp)
	if roster := restored.agentRoster(); len(roster) != 2 || roster[0].Kind != "teammate" || roster[1].Kind != "task" || roster[0].State != "ended at resume" || roster[1].State != "ended at resume" {
		t.Fatalf("restored kind or legacy default: %+v", roster)
	}
	if tree := restored.agentSubtree(); tree.TotalAgents != 4 || tree.Threads[0].Kind != "teammate" || tree.Threads[1].Kind != "task" || tree.Threads[2].Kind != "task" || tree.Threads[3].Kind != "task" {
		t.Fatalf("archive kind or legacy default: %+v", tree)
	}
	for _, name := range []string{"partner", "legacy"} {
		if _, err := restored.sendAgent(context.Background(), agentTargetArgs{Name: name}, "no restart"); err == nil {
			t.Fatalf("resume restarted %s", name)
		}
	}
}

func TestAgents_KindPolicyReachesFreshAndNativeForkRequests(t *testing.T) {
	for _, kind := range []string{"task", "teammate"} {
		for _, fork := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/fork=%t", kind, fork), func(t *testing.T) {
				s := agentFixture(t)
				var child ModelRequest
				s.agentModel = func(Config, *Trace) (Model, error) {
					return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
						child = req
						return contextResponse(openaiName, textBlock("done")), nil
					}}, nil
				}
				raw, err := json.Marshal(spawnArgs{Task: "inspect", Name: "worker", Kind: agentKind(kind), Context: &agentContext{Fork: agentFork(fork)}})
				if err != nil {
					t.Fatal(err)
				}
				s.model = NewScriptedModel(turn(callBlock("spawn", "spawn", string(raw)), callBlock("wait", "wait", `{"ids":["worker"]}`)), turn(textBlock("done")))
				agentTurn(t, s, "inspect independently")
				policy := child.Instructions
				if fork {
					policy = child.History[len(child.History)-1].User.Text
				}
				want := "Per-task limits: 64 steps, 256 calls, 400000 reported uncached-input plus output tokens."
				if kind == "teammate" {
					want = "There are no step, tool-call or reported-token caps."
				}
				if !strings.Contains(policy, "Agent kind: "+kind+".") || !strings.Contains(policy, want) || !strings.Contains(policy, "Only the root may spawn teammates; you may spawn only tasks.") {
					t.Fatalf("wrong child policy: %s", policy)
				}
				wantMode := "fresh"
				if fork {
					wantMode = "fork_native"
				}
				if s.agentSubtree().Threads[0].ContextMode != wantMode {
					t.Fatalf("wrong context mode: %+v", s.agentSubtree())
				}
			})
		}
	}
}
