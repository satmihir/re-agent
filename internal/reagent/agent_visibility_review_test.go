package reagent

import (
	"context"
	"io"
	"strings"
	"sync"
	"testing"
)

func TestAgents_SubtreeRetainsPartiallyKnownUsage(t *testing.T) {
	s := agentFixture(t)
	s.agents.nodes["t1"] = &agentThread{AgentThread: AgentThread{ID: "t1", ParentID: "root", Cost: Usage{Known: true}}, depth: 1}
	child := &Session{agents: s.agents, agentID: "t1"}
	r := Run{session: child, steps: 1}
	r.meterAgent(Usage{Known: true, InputTokens: 30, CachedInputTokens: 10, OutputTokens: 5})
	r.meterAgent(Usage{})
	for _, collected := range []bool{false, true} {
		if collected {
			s.agents.mu.Lock()
			s.agents.retireLocked("t1")
			s.agents.mu.Unlock()
		}
		tree := s.agentSubtree()
		if tree.TotalCost != s.agents.usage() || tree.TotalCost.Known || tree.TotalCost.InputTokens != 30 || tree.TotalCost.OutputTokens != 5 {
			t.Fatalf("partial cost lost %+v", tree)
		}
	}
}

func TestAgents_ResumePreservesSubtreeAccounting(t *testing.T) {
	s := agentFixture(t)
	store, err := openSessionStore(s.ID, true)
	if err != nil {
		t.Fatal(err)
	}
	defer store.close()
	s.store = store
	s.agents.nodes["t1"] = &agentThread{AgentThread: AgentThread{ID: "t1", ParentID: "root", Name: "tests", State: "idle", Cost: Usage{Known: true, InputTokens: 10}}, depth: 1}
	s.agents.nodes["t2"] = &agentThread{AgentThread: AgentThread{ID: "t2", ParentID: "t1", State: "running", Cost: Usage{Known: true, InputTokens: 20}}, depth: 2}
	s.agents.archive["t3"] = agentObservation{AgentThread: AgentThread{ID: "t3", ParentID: "root", State: "dismissed", Cost: Usage{Known: false, InputTokens: 30}}, Depth: 1}
	if err := s.checkpoint("idle", ""); err != nil {
		t.Fatal(err)
	}
	cp, err := store.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	resumed := NewSession(s.cfg, NewScriptedModel(), NewTrace(io.Discard), io.Discard)
	resumed.restore(cp)
	t.Cleanup(resumed.closeAgents)
	tree := resumed.agentSubtree()
	if len(tree.Threads) != 3 || !tree.AccountingComplete || tree.TotalCost.InputTokens != 60 || tree.TotalCost != resumed.agents.usage() || resumed.agents.next != 3 {
		t.Fatalf("resume lost accounting %+v", tree)
	}
	if tree.Threads[0].Depth != 1 || tree.Threads[1].Depth != 2 || tree.Threads[0].State != "ended at resume" || tree.Threads[1].State != "ended at resume" || !tree.Threads[1].EffectsUnknown {
		t.Fatalf("depth/state %+v", tree.Threads)
	}
	legacy := NewSession(s.cfg, NewScriptedModel(), NewTrace(io.Discard), io.Discard)
	legacy.restore(chatCheckpoint{Agents: true, AgentRoster: cp.AgentRoster})
	if legacy.agentSubtree().AccountingComplete || legacy.agentSubtree().Threads[0].Depth != 1 {
		t.Fatal("legacy checkpoint claims complete accounting")
	}
}

func TestAgents_DisplayNameReuseMatchesWait(t *testing.T) {
	s := agentFixture(t)
	s.restore(chatCheckpoint{Agents: true, AgentRoster: []AgentThread{{ID: "t1", ParentID: "root", Name: "tests", Cost: Usage{Known: true}}}})
	s.agents.nodes["t2"] = &agentThread{AgentThread: AgentThread{ID: "t2", ParentID: "root", Name: "tests", Steps: 7, Cost: Usage{Known: true}}, depth: 1}
	_, label, _ := s.agents.displaySnapshot("root", []string{"tests"})
	if !strings.Contains(label, "t2 tests (step 7)") || strings.Contains(label, "t1") {
		t.Fatalf("wrong reused name %q", label)
	}
	statuses, err := s.waitAgents(context.Background(), []string{"tests"})
	if err != nil || statuses[0].ID != "t2" {
		t.Fatal("wait/display disagree")
	}
}

func TestAgents_CheckpointRosterAndSubtreeShareSnapshot(t *testing.T) {
	s := agentFixture(t)
	s.agents.nodes["t1"] = &agentThread{AgentThread: AgentThread{ID: "t1", ParentID: "root", Cost: Usage{Known: true}}, depth: 1}
	child := &Session{agents: s.agents, agentID: "t1"}
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		for i := 0; i < 1000; i++ {
			r := Run{session: child, steps: i}
			r.meterAgent(Usage{Known: true, InputTokens: 1})
		}
	}()
	for i := 0; i < 1000; i++ {
		roster, subtree := s.agentCheckpoint()
		if roster[0].Cost != subtree.Threads[0].Cost || roster[0].Cost != subtree.TotalCost {
			t.Fatal("checkpoint split across usage update")
		}
	}
	wg.Wait()
}
