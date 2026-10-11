package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func waitForAgentFile(ctx context.Context, path string) error {
	for {
		if _, err := os.Stat(path); err == nil {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(5 * time.Millisecond):
		}
	}
}

func TestAgents_ConcurrentExecNoticesRootAndAgentsWithoutBlocking(t *testing.T) {
	for _, scenario := range []string{"agent-agent", "agent-root", "root-agent"} {
		t.Run(scenario, func(t *testing.T) {
			s := agentFixture(t)
			ready := filepath.Join(s.cfg.WorkspacePath, "ready")
			release := filepath.Join(s.cfg.WorkspacePath, "release")
			slowArgs := `{"argv":["sh","-c","touch ready; while ! test -f release; do sleep 0.01; done"],"cwd":"."}`
			fastArgs := `{"argv":["printf","overlap"],"cwd":"."}`
			check := func(history []Entry, other string) {
				for _, entry := range history {
					if entry.Tool == nil || entry.Tool.Name != "exec" {
						continue
					}
					var result execResult
					if err := json.Unmarshal(entry.Tool.Outcome.Data, &result); err != nil {
						t.Error(err)
						return
					}
					if len(result.ConcurrentExec) != 1 || result.ConcurrentExec[0].AgentID != other || len(result.ConcurrentExec[0].Argv) != 3 || result.ConcurrentExec[0].Argv[0] != "sh" || result.Stdout != "overlap" {
						t.Errorf("missing overlap notice: %+v", result)
					}
					return
				}
				t.Error("no exec result")
			}
			created := 0
			s.agentModel = func(Config, *Trace) (Model, error) {
				created++
				if scenario != "root-agent" && created == 1 {
					return NewScriptedModel(turn(callBlock("slow", "exec", slowArgs)), turn(textBlock("slow done"))), nil
				}
				return agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
					if req.Scope.Step == 1 {
						if err := waitForAgentFile(ctx, ready); err != nil {
							return ModelResponse{}, err
						}
						return turn(callBlock("fast", "exec", fastArgs)), nil
					}
					other := "t1"
					if scenario == "root-agent" {
						other = "root"
					}
					check(req.History, other)
					if err := os.WriteFile(release, nil, 0600); err != nil {
						t.Error(err)
					}
					return turn(textBlock("fast done")), nil
				}}, nil
			}
			step := 0
			s.model = agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
				step++
				switch step {
				case 1:
					if scenario == "agent-agent" {
						return turn(writeSpawnBlock("slow", "slow", "slow"), writeSpawnBlock("fast", "fast", "fast"), callBlock("wait", "wait", `{"ids":["slow","fast"]}`)), nil
					}
					if scenario == "root-agent" {
						return turn(writeSpawnBlock("fast", "fast", "fast"), callBlock("slow", "exec", slowArgs), callBlock("wait", "wait", `{"ids":["fast"]}`)), nil
					}
					return turn(writeSpawnBlock("slow", "slow", "slow")), nil
				case 2:
					if scenario == "agent-root" {
						if err := waitForAgentFile(ctx, ready); err != nil {
							return ModelResponse{}, err
						}
						return turn(callBlock("fast", "exec", fastArgs)), nil
					}
				case 3:
					if scenario == "agent-root" {
						check(req.History, "t1")
						if err := os.WriteFile(release, nil, 0600); err != nil {
							t.Error(err)
						}
						return turn(callBlock("wait", "wait", `{"ids":["slow"]}`)), nil
					}
				}
				return turn(textBlock("done")), nil
			}}
			agentTurn(t, s, "exec overlap")
			s.agents.mu.Lock()
			if len(s.agents.execs) != 0 {
				t.Error("exec registrations leaked")
			}
			s.agents.mu.Unlock()
			// A later command must have no spurious overlap notice.
			s.model = NewScriptedModel(turn(callBlock("later", "exec", fastArgs)), turn(textBlock("done")))
			agentTurn(t, s, "later command")
			got := results(s)
			if strings.Contains(string(got[len(got)-1].Outcome.Data), "concurrent_exec") {
				t.Fatal("stale notice after exec ended")
			}
		})
	}
}

func TestAgents_TwoWritingAgentsReadSameVersionSecondEditIsStale(t *testing.T) {
	s := agentFixture(t)
	if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, "shared"), []byte("original"), 0600); err != nil {
		t.Fatal(err)
	}
	read := make(chan struct{}, 2)
	begin, firstDone := make(chan struct{}), make(chan struct{})
	created := 0
	s.agentModel = func(Config, *Trace) (Model, error) {
		created++
		first := created == 1
		return agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
			switch req.Scope.Step {
			case 1:
				return turn(callBlock("read", "read_file", `{"path":"shared"}`)), nil
			case 2:
				read <- struct{}{}
				select {
				case <-begin:
				case <-ctx.Done():
					return ModelResponse{}, ctx.Err()
				}
				if !first {
					select {
					case <-firstDone:
					case <-ctx.Done():
						return ModelResponse{}, ctx.Err()
					}
				}
				return turn(callBlock("edit", "edit_file", `{"path":"shared","old_text":"original","new_text":"winner"}`)), nil
			default:
				out := req.History[len(req.History)-1].Tool.Outcome
				if first {
					if !out.OK {
						t.Errorf("first: %+v", out)
					}
					close(firstDone)
				} else if out.Code != "stale_file" {
					t.Errorf("second: %+v", out)
				}
				return turn(textBlock("done")), nil
			}
		}}, nil
	}
	step := 0
	s.model = agentTestModel{generate: func(ctx context.Context, _ ModelRequest) (ModelResponse, error) {
		step++
		if step == 1 {
			return turn(writeSpawnBlock("first", "first", "first"), writeSpawnBlock("second", "second", "second")), nil
		}
		if step == 2 {
			for i := 0; i < 2; i++ {
				select {
				case <-read:
				case <-ctx.Done():
					return ModelResponse{}, ctx.Err()
				}
			}
			close(begin)
			return turn(callBlock("wait", "wait", `{"ids":["first","second"]}`)), nil
		}
		return turn(textBlock("done")), nil
	}}
	result := agentTurn(t, s, "same version")
	if len(result.Effects) != 1 {
		t.Fatalf("effects: %+v", result.Effects)
	}
}

func TestAgents_AbnormalRootExitStillIncludesLateChildEffects(t *testing.T) {
	s := agentFixture(t)
	started := make(chan struct{})
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
			if req.Scope.Step == 1 {
				return turn(callBlock("create", "write_file", `{"path":"before-cancel","content":"persisted"}`)), nil
			}
			close(started)
			<-ctx.Done()
			return ModelResponse{}, ctx.Err()
		}}, nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	step := 0
	s.model = agentTestModel{generate: func(ctx context.Context, _ ModelRequest) (ModelResponse, error) {
		step++
		if step == 1 {
			return turn(writeSpawnBlock("spawn", "write", "writer")), nil
		}
		select {
		case <-started:
		case <-ctx.Done():
			return ModelResponse{}, ctx.Err()
		}
		cancel()
		return ModelResponse{}, ctx.Err()
	}}
	result, err := s.Turn(ctx, "cancel", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
	if err != nil || result.Status != StatusCancelled || len(result.Effects) != 1 || result.Effects[0].Path != "before-cancel" {
		t.Fatalf("late effects lost: %+v %v", result, err)
	}
}

func TestAgents_SwitchedAndAliasedPathsKeepSharedPublicationGuard(t *testing.T) {
	s := agentFixture(t)
	root := s.cfg.WorkspacePath
	if err := os.Mkdir(filepath.Join(root, "sub"), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "sub", "file"), []byte("original"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("sub", filepath.Join(root, "alias")); err != nil {
		t.Fatal(err)
	}
	s.model = NewScriptedModel(turn(writeSpawnBlock("spawn", "writer", "writer"), callBlock("wait", "wait", `{"ids":["writer"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "prepare writer")
	child := s.agents.nodes["t1"].session.workspace.active
	destination, bad := workspaceDestinationAt(context.Background(), filepath.Join(root, "sub"), false)
	if bad != nil {
		t.Fatal(bad)
	}
	out, err := s.selectWorkspace(context.Background(), destination)
	if err != nil || !out.OK {
		t.Fatalf("switch: %+v %v", out, err)
	}
	if s.workspace.active.publishMu != child.publishMu {
		t.Fatal("switch lost tree publication guard")
	}
	for _, pair := range []struct {
		ws   *Workspace
		path string
	}{{s.workspace.active, "file"}, {child, "alias/file"}} {
		args, _ := json.Marshal(map[string]string{"path": pair.path})
		out, err := NewReadFileTool(pair.ws).Execute(context.Background(), args)
		if err != nil || !out.OK {
			t.Fatalf("read: %+v %v", out, err)
		}
	}
	out, err = NewWriteFileTool(s.workspace.active).Execute(context.Background(), json.RawMessage(`{"path":"file","content":"root changed"}`))
	if err != nil || !out.OK {
		t.Fatalf("root write: %+v %v", out, err)
	}
	out, err = NewDeleteFileTool(child).Execute(context.Background(), json.RawMessage(`{"path":"alias/file"}`))
	if err != nil || out.Code != "stale_file" {
		t.Fatalf("aliased delete: %+v %v", out, err)
	}
}

func TestAgents_CancelledFileWriterWaitingForGuardDoesNotPublish(t *testing.T) {
	s := agentFixture(t)
	ctx, cancel := context.WithCancel(context.Background())
	s.cfg.Workspace.publishMu.Lock()
	done := make(chan ToolOutcome, 1)
	go func() {
		out, err := NewWriteFileTool(s.cfg.Workspace).Execute(ctx, json.RawMessage(`{"path":"cancelled","content":"bad"}`))
		if err != nil {
			t.Error(err)
		}
		done <- out
	}()
	cancel()
	s.cfg.Workspace.publishMu.Unlock()
	if out := <-done; out.Code != "not_executed" {
		t.Fatalf("cancelled writer: %+v", out)
	}
	if _, err := os.Stat(filepath.Join(s.cfg.WorkspacePath, "cancelled")); !os.IsNotExist(err) {
		t.Fatal("cancelled writer published")
	}
}

func TestAgents_DismissAndResetKeepUndeliveredEffects(t *testing.T) {
	for _, action := range []string{"dismiss", "reset"} {
		t.Run(action, func(t *testing.T) {
			s := agentFixture(t)
			s.agentModel = func(Config, *Trace) (Model, error) {
				return NewScriptedModel(turn(callBlock("create", "write_file", `{"path":"before-control","content":"survives"}`)), turn()), nil
			}
			args := `{"name":"writer"}`
			s.model = NewScriptedModel(turn(writeSpawnBlock("spawn", "write", "writer"), callBlock("wait", "wait", `{"ids":["writer"]}`), callBlock("control", action, args)), turn(textBlock("done")))
			result := agentTurn(t, s, action)
			if len(result.Effects) != 1 || result.Effects[0].Path != "before-control" {
				t.Fatalf("control lost effects: %+v", result)
			}
		})
	}
}

func TestAgents_ConcurrentExecChainKeepsBoundedNoticesForEveryStep(t *testing.T) {
	s := agentFixture(t)
	created := 0
	s.agentModel = func(Config, *Trace) (Model, error) {
		created++
		argv := []string{"sh", "-c", fmt.Sprintf("touch ready-%d; while ! test -f release; do sleep 0.01; done", created), "unused", strings.Repeat("<", 16000)}
		for i := 0; i < 6000; i++ {
			argv = append(argv, "unused")
		}
		args, _ := json.Marshal(map[string]any{"argv": argv, "cwd": "."})
		return NewScriptedModel(turn(callBlock("slow", "exec", string(args))), turn(textBlock("done"))), nil
	}
	step := 0
	s.model = agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
		step++
		if step == 1 {
			var calls []OutputBlock
			for i := 1; i <= 8; i++ {
				calls = append(calls, writeSpawnBlock(fmt.Sprintf("spawn-%d", i), "slow", fmt.Sprintf("slow-%d", i)))
			}
			return turn(calls...), nil
		}
		if step == 2 {
			for i := 1; i <= 8; i++ {
				if err := waitForAgentFile(ctx, filepath.Join(s.cfg.WorkspacePath, fmt.Sprintf("ready-%d", i))); err != nil {
					return ModelResponse{}, err
				}
			}
			return turn(callBlock("chain", "exec", `{"argv":["printf","one"],"then":[["printf","two"],["printf","three"],["printf","four"],["printf","five"],["printf","six"],["printf","seven"],["printf","eight"]],"cwd":"."}`)), nil
		}
		if step == 3 {
			out := req.History[len(req.History)-1].Tool.Outcome
			if encodedSize(out) > MaxResultBytes {
				t.Errorf("unbounded chain: %d", encodedSize(out))
			}
			var chain execChainResult
			if err := json.Unmarshal(out.Data, &chain); err != nil {
				t.Error(err)
			}
			if len(chain.Steps) != 8 {
				t.Errorf("steps: %d", len(chain.Steps))
			}
			for _, raw := range chain.Steps {
				var result execResult
				if err := json.Unmarshal(raw, &result); err != nil {
					t.Error(err)
					continue
				}
				if len(result.ConcurrentExec) != 8 {
					t.Errorf("notices: %d", len(result.ConcurrentExec))
				}
				for _, notice := range result.ConcurrentExec {
					if !notice.Truncated || !strings.HasPrefix(notice.AgentID, "t") {
						t.Errorf("chain notice: %+v", notice)
					}
				}
			}
			if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, "release"), nil, 0600); err != nil {
				t.Error(err)
			}
			return turn(callBlock("wait", "wait", `{"ids":["slow-1","slow-2","slow-3","slow-4","slow-5","slow-6","slow-7","slow-8"]}`)), nil
		}
		return turn(textBlock("done")), nil
	}}
	agentTurn(t, s, "overlapping chain")
}

func TestAgents_ChildReadsDoNotAuthorizeUnreadParentOrSibling(t *testing.T) {
	s := agentFixture(t)
	if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, "existing"), []byte("original"), 0600); err != nil {
		t.Fatal(err)
	}
	created := 0
	s.agentModel = func(Config, *Trace) (Model, error) {
		created++
		if created == 1 {
			return NewScriptedModel(turn(callBlock("read", "read_file", `{"path":"existing"}`)), turn(textBlock("read done"))), nil
		}
		return NewScriptedModel(turn(callBlock("write", "write_file", `{"path":"existing","content":"unread overwrite"}`)), turn(textBlock("done"))), nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("read", "read", "reader"), callBlock("wait", "wait", `{"ids":["reader"]}`)), turn(callBlock("write", "write_file", `{"path":"existing","content":"unread parent overwrite"}`), writeSpawnBlock("sibling", "write", "writer"), callBlock("w2", "wait", `{"ids":["writer"]}`)), turn(textBlock("done")))
	result := agentTurn(t, s, "read evidence stays local")
	if results(s)[2].Outcome.Code != "invalid_arguments" || results(s.agents.nodes["t2"].session)[0].Outcome.Code != "invalid_arguments" || len(result.Effects) != 0 {
		t.Fatalf("read widened authority: %+v %+v", results(s), result.Effects)
	}
}

func TestAgents_CancelExecKeepsUncertaintyAndRequiresResetBeforeSend(t *testing.T) {
	s := agentFixture(t)
	s.agentModel = func(Config, *Trace) (Model, error) {
		return NewScriptedModel(turn(callBlock("sleep", "exec", `{"argv":["sh","-c","touch ready; sleep 30"],"cwd":"."}`)), turn(callBlock("inspect", "read_file", `{"path":"ready"}`)), turn(textBlock("inspected"))), nil
	}
	step := 0
	s.model = agentTestModel{generate: func(ctx context.Context, _ ModelRequest) (ModelResponse, error) {
		step++
		if step == 1 {
			return turn(writeSpawnBlock("spawn", "sleep", "writer")), nil
		}
		if step == 2 {
			if err := waitForAgentFile(ctx, filepath.Join(s.cfg.WorkspacePath, "ready")); err != nil {
				return ModelResponse{}, err
			}
			return turn(callBlock("cancel", "cancel", `{"name":"writer"}`), callBlock("blocked", "send", `{"name":"writer","message":"inspect"}`), callBlock("reset", "reset", `{"name":"writer"}`), callBlock("send", "send", `{"name":"writer","message":"inspect, do not repeat the command"}`), callBlock("wait", "wait", `{"ids":["writer"]}`)), nil
		}
		return turn(textBlock("done")), nil
	}}
	result := agentTurn(t, s, "cancel command")
	got := results(s)
	if !got[1].Outcome.OK || got[2].Outcome.Code != "send_denied" || !got[3].Outcome.OK || !got[4].Outcome.OK {
		t.Fatalf("cancel recovery: %+v", got)
	}
	all := agentResults(s.history)
	if len(all) != 2 || !all[0].Effects.ExecUnknown || all[1].Effects.ExecUnknown || all[1].Result.Reply != "inspected" || len(result.Effects) != 1 || result.Effects[0].Effect != EffectUnknown {
		t.Fatalf("uncertainty/recovery lost: %+v %+v", all, result.Effects)
	}
	s.agents.mu.Lock()
	defer s.agents.mu.Unlock()
	if len(s.agents.execs) != 0 {
		t.Fatal("cancelled exec registration leaked")
	}
}

func TestAgents_ExecNoticeSurvivesRootSwitchIntoChildWorkingDirectory(t *testing.T) {
	s := agentFixture(t)
	root := s.cfg.WorkspacePath
	if err := os.Mkdir(filepath.Join(root, "sub"), 0700); err != nil {
		t.Fatal(err)
	}
	var tools []Tool
	for _, spec := range s.cfg.Registry.Specs() {
		tool, _ := s.cfg.Registry.Lookup(spec.Name)
		tools = append(tools, tool)
	}
	registry, err := NewRegistry(Mode{}, append(tools, NewSwitchWorkspaceTool())...)
	if err != nil {
		t.Fatal(err)
	}
	s.cfg.Registry = registry.bindWorkspace(s, s.workspace.active)
	s.agentModel = func(Config, *Trace) (Model, error) {
		return NewScriptedModel(turn(callBlock("slow", "exec", `{"argv":["sh","-c","touch ready; while ! test -f release; do sleep 0.01; done"],"cwd":"sub"}`)), turn(textBlock("done"))), nil
	}
	step := 0
	s.model = agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
		step++
		switch step {
		case 1:
			return turn(writeSpawnBlock("spawn", "slow", "slow")), nil
		case 2:
			if err := waitForAgentFile(ctx, filepath.Join(root, "sub", "ready")); err != nil {
				return ModelResponse{}, err
			}
			args, _ := json.Marshal(map[string]string{"path": filepath.Join(root, "sub")})
			return turn(callBlock("switch", "switch_workspace", string(args))), nil
		case 3:
			return turn(callBlock("exec", "exec", `{"argv":["printf","same directory"],"cwd":"."}`)), nil
		case 4:
			var result execResult
			if err := json.Unmarshal(req.History[len(req.History)-1].Tool.Outcome.Data, &result); err != nil {
				t.Error(err)
			}
			if len(result.ConcurrentExec) != 1 || result.ConcurrentExec[0].AgentID != "t1" {
				t.Errorf("switched-root notice: %+v", result)
			}
			if err := os.WriteFile(filepath.Join(root, "sub", "release"), nil, 0600); err != nil {
				t.Error(err)
			}
			return turn(callBlock("wait", "wait", `{"ids":["slow"]}`)), nil
		}
		return turn(textBlock("done")), nil
	}}
	agentTurn(t, s, "same directory through different workspace roots")
}
