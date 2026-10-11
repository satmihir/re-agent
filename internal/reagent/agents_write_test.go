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

func TestAgents_SeparateHandlesDetectSequentialStaleEdit(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "shared"), []byte("original"), 0600); err != nil {
		t.Fatal(err)
	}
	var handles []*Workspace
	for i := 0; i < 2; i++ {
		ws, err := OpenWorkspace(root)
		if err != nil {
			t.Fatal(err)
		}
		handles = append(handles, ws)
		out, err := NewReadFileTool(ws).Execute(context.Background(), json.RawMessage(`{"path":"shared"}`))
		if err != nil || !out.OK {
			t.Fatalf("read: %+v %v", out, err)
		}
	}
	args := json.RawMessage(`{"path":"shared","old_text":"original","new_text":"first"}`)
	first, err := NewEditFileTool(handles[0]).Execute(context.Background(), args)
	if err != nil || !first.OK {
		t.Fatalf("first edit: %+v %v", first, err)
	}
	second, err := NewEditFileTool(handles[1]).Execute(context.Background(), args)
	if err != nil || second.Code != "stale_file" {
		t.Fatalf("second edit: %+v %v", second, err)
	}
}

func TestAgents_ConcurrentPublicationNeverLosesUpdate(t *testing.T) {
	s := agentFixture(t)
	// Actual spawned handles must share the guard, not their observations.
	s.model = NewScriptedModel(turn(writeSpawnBlock("a", "one", "one"), writeSpawnBlock("b", "two", "two"), callBlock("wait", "wait", `{"ids":["one","two"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "prepare handles")
	s.agents.mu.Lock()
	handles := []*Workspace{s.agents.nodes["t1"].session.workspace.active, s.agents.nodes["t2"].session.workspace.active}
	s.agents.mu.Unlock()
	for iteration := 0; iteration < 400; iteration++ {
		path := fmt.Sprintf("shared-%d", iteration)
		if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, path), []byte("original"), 0600); err != nil {
			t.Fatal(err)
		}
		for _, ws := range handles {
			args, _ := json.Marshal(map[string]string{"path": path})
			out, err := NewReadFileTool(ws).Execute(context.Background(), args)
			if err != nil || !out.OK {
				t.Fatalf("read: %+v %v", out, err)
			}
		}
		participants := append([]*Workspace(nil), handles...)
		if iteration%3 == 0 {
			participants[0] = s.workspace.active
			args, _ := json.Marshal(map[string]string{"path": path})
			if out, err := NewReadFileTool(participants[0]).Execute(context.Background(), args); err != nil || !out.OK {
				t.Fatalf("root read: %+v %v", out, err)
			}
		}
		var wg sync.WaitGroup
		start := make(chan struct{})
		outcomes := make([]ToolOutcome, 2)
		for i, ws := range participants {
			wg.Add(1)
			go func(i int, ws *Workspace) {
				defer wg.Done()
				<-start
				tool := NewWriteFileTool(ws)
				fields := map[string]string{"path": path, "content": fmt.Sprintf("writer-%d", i), "expected_sha256": digestOf("original")}
				if iteration%4 == 1 || iteration%4 == 2 && i == 0 {
					tool = NewEditFileTool(ws)
					fields = map[string]string{"path": path, "old_text": "original", "new_text": fmt.Sprintf("writer-%d", i)}
				}
				if iteration%4 == 3 && i == 0 {
					tool = NewDeleteFileTool(ws)
					fields = map[string]string{"path": path}
				}
				args, _ := json.Marshal(fields)
				out, err := tool.Execute(context.Background(), args)
				if err != nil {
					t.Error(err)
				}
				outcomes[i] = out
			}(i, ws)
		}
		close(start)
		wg.Wait()
		applied, stale := 0, 0
		for _, out := range outcomes {
			if out.OK {
				applied++
			}
			if out.Code == "stale_file" || iteration%4 == 3 && out.Code == "not_found" {
				stale++
			}
		}
		if applied != 1 || stale != 1 {
			t.Fatalf("iteration %d silently lost an update: %+v", iteration, outcomes)
		}
		content, err := os.ReadFile(filepath.Join(s.cfg.WorkspacePath, path))
		if iteration%4 == 3 && outcomes[0].OK {
			if !os.IsNotExist(err) {
				t.Fatalf("deleted file recreated: %s %v", content, err)
			}
			continue
		}
		winner := 0
		if outcomes[1].OK {
			winner = 1
		}
		if err != nil || string(content) != fmt.Sprintf("writer-%d", winner) {
			t.Fatalf("winner's contents lost: %s %v", content, err)
		}
	}
}

func writeSpawnBlock(id, task, name string) OutputBlock {
	args, _ := json.Marshal(spawnArgs{Task: task, Name: name, Authority: "write", Context: &agentContext{Brief: "work independently"}})
	return callBlock(id, "spawn", string(args))
}

func TestAgents_WriteEditsReachRootAndRecapWithoutAuthorizingParent(t *testing.T) {
	s := agentFixture(t)
	path := filepath.Join(s.cfg.WorkspacePath, "shared")
	if err := os.WriteFile(path, []byte("original"), 0600); err != nil {
		t.Fatal(err)
	}
	s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
		for _, name := range []string{"edit_file", "write_file", "delete_file", "exec"} {
			if _, found := cfg.Registry.Lookup(name); !found {
				t.Errorf("missing granted tool %s", name)
			}
		}
		return NewScriptedModel(
			turn(callBlock("read", "read_file", `{"path":"shared"}`)),
			turn(callBlock("edit", "edit_file", `{"path":"shared","old_text":"original","new_text":"child"}`), callBlock("create", "write_file", `{"path":"created","content":"new"}`)),
			turn(callBlock("delete", "delete_file", `{"path":"created"}`)),
			turn(textBlock(strings.Repeat("雪\x00\"\\<", 20000)))), nil
	}
	s.model = NewScriptedModel(
		turn(callBlock("read", "read_file", `{"path":"shared"}`), writeSpawnBlock("spawn", "edit", "writer"), callBlock("wait", "wait", `{"ids":["writer"]}`)),
		turn(callBlock("stale", "write_file", `{"path":"shared","content":"parent based on original"}`)),
		turn(callBlock("reread", "read_file", `{"path":"shared"}`)),
		turn(callBlock("edit", "edit_file", `{"path":"shared","old_text":"child","new_text":"parent after reread"}`)),
		turn(textBlock("done")))
	result := agentTurn(t, s, "delegate write")
	if results(s)[3].Outcome.Code != "stale_file" || !results(s)[5].Outcome.OK {
		t.Fatalf("parent digest flow: %+v", results(s))
	}
	all := agentResults(s.history)
	if len(all) != 1 || !all[0].Truncated || len(all[0].Effects.Files) != 3 || all[0].Effects.ExecUnknown {
		t.Fatalf("truncated delivery lost effects: %+v", all)
	}
	if len(result.Effects) != 4 || result.Effects[0].AgentID != "t1" || result.Effects[0].Path != "shared" || result.Effects[0].RunID == "" {
		t.Fatalf("root effects: %+v", result.Effects)
	}
	for _, entry := range s.history {
		if entry.User != nil && entry.User.Source == "agent" {
			raw, _ := json.Marshal(entry.User)
			if len(raw) > MaxResultBytes {
				t.Fatalf("unbounded delivery: %d bytes", len(raw))
			}
		}
	}
	var recap bytes.Buffer
	NewDisplay(&recap).summary(result, 0, false, true)
	for _, text := range []string{"agent t1: update shared", "agent t1: create created", "agent t1: delete created"} {
		if !strings.Contains(recap.String(), text) {
			t.Errorf("recap missing %q: %s", text, recap.String())
		}
	}
	// A named agent retains both write authority and its own latest observations.
	s.model = NewScriptedModel(turn(callBlock("send", "send", `{"name":"writer","message":"continue"}`), callBlock("w2", "wait", `{"ids":["writer"]}`)), turn(textBlock("done")))
	s.agents.nodes["t1"].session.model = NewScriptedModel(turn(callBlock("old", "edit_file", `{"path":"shared","append_text":"!"}`)), turn(textBlock("done")))
	later := agentTurn(t, s, "continue writing")
	if len(later.Effects) != 0 || len(agentResults(s.history)) != 2 {
		t.Fatalf("effects repeated: %+v", later)
	}
	child := s.agents.nodes["t1"].session
	if got := results(child); got[len(got)-1].Outcome.Code != "stale_file" {
		t.Fatalf("child missed parent changes: %+v", got)
	}
}

func TestAgents_WriteAuthorityOnlyShrinks(t *testing.T) {
	for _, scenario := range []struct {
		name                 string
		authority            string
		readOnly, plan       bool
		spawnCode, writeCode string
	}{
		{"default-read", "", false, false, "ok", "tool_unavailable"},
		{"explicit-read", "read", false, false, "ok", "tool_unavailable"},
		{"read-only-parent", "write", true, false, "spawn_denied", ""},
		{"plan-parent", "write", false, true, "ok", "plan_mode"},
		{"invalid-authority", "admin", false, false, "invalid_arguments", ""},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			s := agentFixture(t)
			if scenario.readOnly {
				registry, err := NewRegistry(Mode{ReadOnly: true}, append(NewAgentTools(), NewReadFileTool(s.cfg.Workspace), NewWriteFileTool(s.cfg.Workspace))...)
				if err != nil {
					t.Fatal(err)
				}
				s.cfg.Registry = registry.bindWorkspace(s, s.workspace.active)
			}
			s.planMode = scenario.plan
			s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
				if cfg.PlanMode != scenario.plan {
					t.Error("lost inherited plan mode")
				}
				return NewScriptedModel(turn(callBlock("write", "write_file", `{"path":"forbidden","content":"bad"}`)), turn(textBlock("done"))), nil
			}
			args, _ := json.Marshal(spawnArgs{Task: "try write", Name: "child", Authority: scenario.authority, Context: &agentContext{}})
			responses := []ModelResponse{turn(callBlock("spawn", "spawn", string(args)))}
			if scenario.spawnCode == "ok" {
				responses = append(responses, turn(callBlock("wait", "wait", `{"ids":["child"]}`)))
			}
			responses = append(responses, turn(textBlock("done")))
			s.model = NewScriptedModel(responses...)
			agentTurn(t, s, "authority")
			if results(s)[0].Outcome.Code != scenario.spawnCode {
				t.Fatalf("spawn: %+v", results(s))
			}
			if scenario.spawnCode == "ok" {
				child := s.agents.nodes["t1"].session
				if results(child)[0].Outcome.Code != scenario.writeCode {
					t.Fatalf("write: %+v", results(child))
				}
			}
			if _, err := os.Stat(filepath.Join(s.cfg.WorkspacePath, "forbidden")); !os.IsNotExist(err) {
				t.Fatal("unauthorized file created")
			}
		})
	}
}

func TestAgents_WriteChildGetsOnlyParentsAvailableToolsAndNoWorkspaceOrGitTools(t *testing.T) {
	s := agentFixture(t)
	registry, err := NewRegistry(Mode{}, append(NewAgentTools(), NewReadFileTool(s.cfg.Workspace), NewWriteFileTool(s.cfg.Workspace), NewRequestWorkspaceAccessTool(), NewSwitchWorkspaceTool())...)
	if err != nil {
		t.Fatal(err)
	}
	// An active root-only capability must not leak through spawn's subset selection.
	registry.byName["git_do"] = NewEchoTool()
	registry.specs = append(registry.specs, ToolSpec{Name: "git_do"})
	s.cfg.Registry = registry.bindWorkspace(s, s.workspace.active)
	s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
		for _, name := range []string{"edit_file", "delete_file", "exec", "git_do", "switch_workspace", "request_workspace_access"} {
			if _, found := cfg.Registry.Lookup(name); found {
				t.Errorf("widened authority: %s", name)
			}
		}
		if _, found := cfg.Registry.Lookup("write_file"); !found {
			t.Error("lost available writer")
		}
		return NewScriptedModel(turn(writeSpawnBlock("nested", "nested", "nested"), callBlock("wait", "wait", `{"ids":["nested"]}`)), turn(textBlock("done"))), nil
	}
	// The grandchild uses a leaf model; distinguish its factory invocation.
	factory := s.agentModel
	created := 0
	s.agentModel = func(cfg Config, trace *Trace) (Model, error) {
		created++
		if created == 2 {
			for _, name := range []string{"exec", "git_do", "switch_workspace"} {
				if _, found := cfg.Registry.Lookup(name); found {
					t.Errorf("grandchild gained %s", name)
				}
			}
			return NewScriptedModel(turn(callBlock("create", "write_file", `{"path":"grandchild","content":"new"}`)), turn(textBlock("done"))), nil
		}
		return factory(cfg, trace)
	}
	s.model = NewScriptedModel(turn(writeSpawnBlock("spawn", "subset", "child"), callBlock("wait", "wait", `{"ids":["child"]}`)), turn(textBlock("done")))
	result := agentTurn(t, s, "subset")
	if len(result.Effects) != 1 || result.Effects[0].AgentID != "t2" {
		t.Fatalf("nested effects: %+v", result.Effects)
	}
	all := agentResults(s.history)
	if len(all) != 1 || len(all[0].Effects.Files) != 1 || all[0].Effects.Files[0].Path != "grandchild" {
		t.Fatalf("nested delivery: %+v", all)
	}
}

func TestAgents_ParentPlanModeRefusesSendingNewWorkToExistingWriter(t *testing.T) {
	s := agentFixture(t)
	s.model = NewScriptedModel(turn(writeSpawnBlock("spawn", "write", "writer"), callBlock("wait", "wait", `{"ids":["writer"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "write phase")
	s.planMode = true
	s.model = NewScriptedModel(turn(callBlock("send", "send", `{"name":"writer","message":"write again"}`)), turn(textBlock("done")))
	agentTurn(t, s, "planning phase")
	if results(s)[2].Outcome.Code != "send_denied" {
		t.Fatalf("plan bypass: %+v", results(s))
	}
}

func TestAgents_FileEffectsBoundedBeforeReplyAndExecUncertaintyKept(t *testing.T) {
	s := agentFixture(t)
	s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
		tool, _ := cfg.Registry.Lookup("exec")
		fast := tool.(execTool)
		fast.minTimeout = 0
		cfg.Registry.byName["exec"] = fast
		var calls []OutputBlock
		for i := 0; i < 100; i++ {
			args, _ := json.Marshal(map[string]string{"path": fmt.Sprintf("%03d-%s", i, strings.Repeat("雪\"\\", 50)), "content": "new"})
			calls = append(calls, callBlock(fmt.Sprintf("create-%d", i), "write_file", string(args)))
		}
		return NewScriptedModel(turn(calls...), turn(callBlock("timeout", "exec", `{"argv":["sleep","30"],"cwd":".","timeout_ms":1}`)), turn(textBlock(strings.Repeat("large reply", 10000)))), nil
	}
	s.model = NewScriptedModel(turn(writeSpawnBlock("spawn", "large effects", "writer"), callBlock("wait", "wait", `{"ids":["writer"]}`)), turn(textBlock("done")))
	result := agentTurn(t, s, "large effects")
	all := agentResults(s.history)
	if len(all) != 1 || !all[0].Truncated || !all[0].Effects.ExecUnknown || all[0].Effects.Omitted == 0 || len(all[0].Effects.Files)+all[0].Effects.Omitted != 100 {
		t.Fatalf("bounded effects: %+v", all)
	}
	if len(result.Effects) != 101 || result.Effects[100].Effect != EffectUnknown {
		t.Fatalf("root lost full effects: %+v", result.Effects)
	}
	for _, e := range s.history {
		if e.User != nil && e.User.Source == "agent" {
			raw, _ := json.Marshal(e.User)
			if len(raw) > MaxResultBytes {
				t.Fatalf("entry %d bytes", len(raw))
			}
		}
	}
}

func TestAgents_ResumeWorkingWriterMarksUnknownEffectsInNextChatRoster(t *testing.T) {
	s := agentFixture(t)
	s.store = testSessionStore(t, s.ID)
	started, release := make(chan struct{}), make(chan struct{})
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
			if req.Scope.Step == 1 {
				return turn(callBlock("create", "write_file", `{"path":"left-behind","content":"may survive"}`)), nil
			}
			close(started)
			select {
			case <-release:
			case <-ctx.Done():
				return ModelResponse{}, ctx.Err()
			}
			return turn(textBlock("done")), nil
		}}, nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_, err := s.spawnAgent(ctx, spawnArgs{Task: "working", Name: "writer", Authority: "write", Context: &agentContext{}})
	if err != nil {
		t.Fatal(err)
	}
	select {
	case <-started:
	case <-ctx.Done():
		t.Fatal("writer did not start")
	}
	if err := s.checkpoint("idle", ""); err != nil {
		t.Fatal(err)
	}
	cp, err := s.store.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	close(release)
	s.stopAgents()
	resumed := NewSession(s.cfg, NewScriptedModel(turn(textBlock("inspect first"))), NewTrace(io.Discard), io.Discard)
	resumed.restore(cp)
	agentTurn(t, resumed, "resume working writer")
	roster := resumed.agentRoster()
	if len(roster) != 1 || roster[0].State != "ended at resume" || !roster[0].EffectsUnknown {
		t.Fatalf("resume: %+v", roster)
	}
	user := resumed.history[len(resumed.history)-2].User
	if !strings.Contains(string(user.Roster), `"effects_unknown":true`) {
		t.Fatalf("next chat message lacks uncertainty: %s", user.Roster)
	}
	if _, err := os.Stat(filepath.Join(s.cfg.WorkspacePath, "left-behind")); err != nil {
		t.Fatal(err)
	}
	checkWritingAgentCLIResume(t, s)
	// A second resume must not clear the uncertainty tombstone.
	resumed.store = testSessionStore(t, resumed.ID)
	if err := resumed.checkpoint("idle", ""); err != nil {
		t.Fatal(err)
	}
	cp, err = resumed.store.load(resumed.ID)
	if err != nil {
		t.Fatal(err)
	}
	resumed.restore(cp)
	if !resumed.agentRoster()[0].EffectsUnknown {
		t.Fatal("second resume cleared uncertainty")
	}
}

func TestAgents_DeletionMakesOmittedDigestOverwriteStaleUntilAbsenceRead(t *testing.T) {
	for _, writer := range []string{"root", "sibling"} {
		t.Run(writer, func(t *testing.T) {
			s := agentFixture(t)
			path := filepath.Join(s.cfg.WorkspacePath, "shared")
			if err := os.WriteFile(path, []byte("original"), 0600); err != nil {
				t.Fatal(err)
			}
			recreate := []ModelResponse{
				turn(callBlock("stale", "write_file", `{"path":"shared","content":"based on original"}`)),
				turn(callBlock("absent", "read_file", `{"path":"shared"}`)),
				turn(callBlock("recreate", "write_file", `{"path":"shared","content":"intentional recreation"}`)),
				turn(textBlock("done")),
			}
			created := 0
			s.agentModel = func(Config, *Trace) (Model, error) {
				created++
				if writer == "sibling" && created == 1 {
					responses := []ModelResponse{turn(callBlock("read", "read_file", `{"path":"shared"}`)), turn(textBlock("read done"))}
					return NewScriptedModel(append(responses, recreate...)...), nil
				}
				return NewScriptedModel(turn(callBlock("read", "read_file", `{"path":"shared"}`), callBlock("delete", "delete_file", `{"path":"shared"}`)), turn(textBlock("deleted"))), nil
			}
			first := turn(callBlock("read", "read_file", `{"path":"shared"}`))
			if writer == "sibling" {
				first = turn(writeSpawnBlock("reader", "read", "writer"), callBlock("w1", "wait", `{"ids":["writer"]}`))
			}
			responses := []ModelResponse{first, turn(writeSpawnBlock("deleter", "delete", "deleter"), callBlock("w2", "wait", `{"ids":["deleter"]}`))}
			if writer == "root" {
				responses = append(responses, recreate...)
			} else {
				responses = append(responses, turn(callBlock("send", "send", `{"name":"writer","message":"recreate"}`), callBlock("w3", "wait", `{"ids":["writer"]}`)), turn(textBlock("done")))
			}
			s.model = NewScriptedModel(responses...)
			agentTurn(t, s, "deletion conflict")
			target := s
			if writer == "sibling" {
				target = s.agents.nodes["t1"].session
			}
			got := results(target)
			last := got[len(got)-3:]
			if last[0].Outcome.Code != "stale_file" || last[1].Outcome.Code != "not_found" || !last[2].Outcome.OK {
				t.Fatalf("deletion flow: %+v", last)
			}
			content, err := os.ReadFile(path)
			if err != nil || string(content) != "intentional recreation" {
				t.Fatalf("recreation: %s %v", content, err)
			}
		})
	}
}
