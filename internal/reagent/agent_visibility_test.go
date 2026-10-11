package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestAgents_DefaultWaitDoesNotAdvanceResearch(t *testing.T) {
	s := agentFixture(t)
	release := make(chan struct{})
	started := make(chan struct{})
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(ctx context.Context, _ ModelRequest) (ModelResponse, error) {
			close(started)
			select {
			case <-release:
				return turn(textBlock("research complete")), nil
			case <-ctx.Done():
				return ModelResponse{}, ctx.Err()
			}
		}}, nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("spawn", "research", "research")), turn(callBlock("wait", "wait", `{"ids":["research"]}`)), turn(textBlock("used research")))
	done := make(chan RunResult, 1)
	go func() { done <- agentTurn(t, s, "research") }()
	<-started
	select {
	case <-done:
		t.Fatal("parent advanced before research completed")
	case <-time.After(30 * time.Millisecond):
	}
	ctx, cancel := agentWaitContext(context.Background(), -1)
	if _, has := ctx.Deadline(); has {
		t.Error("omitted timeout has deadline")
	}
	cancel()
	close(release)
	result := <-done
	if result.Reply != "used research" || len(agentResults(s.history)) != 1 {
		t.Fatalf("result %+v", result)
	}
}

func TestAgents_SubtreeAccountsCollectedNestedAgents(t *testing.T) {
	s := agentFixture(t)
	var mu sync.Mutex
	created := 0
	s.agentModel = func(Config, *Trace) (Model, error) {
		mu.Lock()
		created++
		number := created
		mu.Unlock()
		if number == 1 {
			return NewScriptedModel(turn(spawnBlock("nested", "nested", "")), turn(callBlock("nested-wait", "wait", `{"ids":["t2"]}`)), turn(textBlock("parent done"))), nil
		}
		return NewScriptedModel(turn(textBlock("nested done"))), nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("outer", "outer", "")), turn(callBlock("outer-wait", "wait", `{"ids":["t1"]}`)), turn(callBlock("tree", "threads", `{"subtree":true}`)), turn(textBlock("done")))
	result := agentTurn(t, s, "nested")
	var tree agentSubtree
	if err := json.Unmarshal(results(s)[2].Outcome.Data, &tree); err != nil {
		t.Fatal(err)
	}
	if len(tree.Threads) != 2 || len(s.agentRoster()) != 0 {
		t.Fatalf("tree %+v", tree)
	}
	if tree.Threads[0].ParentID != "root" || tree.Threads[0].Depth != 1 || tree.Threads[1].ParentID != "t1" || tree.Threads[1].Depth != 2 {
		t.Fatalf("parents/depth %+v", tree.Threads)
	}
	sum := Usage{Known: true}
	for _, n := range tree.Threads {
		sum.Add(n.Cost)
	}
	if sum != tree.TotalCost || sum != s.agents.usage() || result.TreeCost.InputTokens != sum.InputTokens+result.Usage.InputTokens {
		t.Fatalf("cost %+v %+v %+v", sum, tree.TotalCost, result)
	}
}

func TestAgents_VisibilitySnapshotConcurrentMetering(t *testing.T) {
	s := agentFixture(t)
	s.agents.mu.Lock()
	s.agents.nodes["t1"] = &agentThread{AgentThread: AgentThread{ID: "t1", ParentID: "root", State: "running", Cost: Usage{Known: true}}, depth: 1, working: true}
	s.agents.nodes["t2"] = &agentThread{AgentThread: AgentThread{ID: "t2", ParentID: "t1", State: "running", Cost: Usage{Known: true}}, depth: 2, working: true}
	s.agents.mu.Unlock()
	// Synthetic workers exercise the same meter as a running Turn without sharing Sessions.
	child := &Session{agents: s.agents, agentID: "t2"}
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		for i := 0; i < 1000; i++ {
			r := Run{session: child, steps: i}
			r.meterAgent(Usage{Known: true, InputTokens: 1, OutputTokens: 1})
		}
	}()
	for i := 0; i < 1000; i++ {
		count, label, _ := s.agents.displaySnapshot("root", []string{"t1"})
		tree := s.agentSubtree()
		if count != 2 || !strings.Contains(label, "step") || len(tree.Threads) != 2 {
			t.Fatal("snapshot missing descendants")
		}
	}
	wg.Wait()
	tree := s.agentSubtree()
	if tree.TotalCost != s.agents.usage() {
		t.Fatal("cost mismatch")
	}
	s.agents.mu.Lock()
	s.agents.nodes["t1"].working = false
	s.agents.nodes["t2"].working = false
	s.agents.mu.Unlock()
}

func TestAgents_PlainNoticesAndExplicitTimeout(t *testing.T) {
	s := agentFixture(t)
	var output bytes.Buffer
	s.display = NewDisplay(&output)
	s.display.agents = s.agents
	s.model = NewScriptedModel(turn(spawnBlock("a", "research", "")), turn(callBlock("w", "wait", `{"ids":["t1"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "go")
	text := output.String()
	if !strings.Contains(text, "finished · completed") || !strings.Contains(text, "results delivered") || strings.Contains(text, "\x1b") || strings.Contains(text, "waiting on") {
		t.Fatalf("plain output %q", text)
	}
	ctx, cancel := agentWaitContext(context.Background(), 0)
	defer cancel()
	if ctx.Err() != context.DeadlineExceeded {
		t.Fatal("explicit zero timeout no longer polls")
	}
}
