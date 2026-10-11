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
	"sync"
	"testing"
	"time"
)

func TestChat_ReviewLevelsAreOrdinaryTurns(t *testing.T) {
	for _, level := range []string{"", "low", "medium", "high", "max"} {
		t.Run("level_"+level, func(t *testing.T) {
			reply := turn(textBlock("Review complete."))
			reply.Usage = Usage{Known: true, InputTokens: 12, OutputTokens: 3}
			model := &compactModel{replies: []ModelResponse{reply, turn(textBlock("next reply"))}}
			cfg := testConfig(t)
			cfg.PlanMode = true
			s := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
			s.snapshot = func(context.Context, string, bool) json.RawMessage {
				return json.RawMessage(`{"kind":"workspace_state","date":"2026-10-11"}`)
			}
			c := &conversation{session: s, traceDir: t.TempDir(), usage: Usage{Known: true}}
			var out, errs bytes.Buffer
			if code := chat(context.Background(), c, newLineReader(strings.NewReader("/review "+level+"\nnext question\n/exit\n"), &errs, nil), &out, &errs); code != exitOK {
				t.Fatalf("exit %d: %s", code, &errs)
			}
			wantLevel := level
			if wantLevel == "" {
				wantLevel = "medium"
			}
			want := "re:agent code review\nReview level: " + wantLevel + "\n\n" + reviewPrompt
			user := model.requests[0].History[0].User
			if len(model.requests) != 2 || user.Text != want || len(user.Workspace) == 0 || user.Plan != "on" || !s.planMode || s.Turns() != 2 || c.usage.InputTokens != 12 || out.String() != "Review complete.\nnext reply\n" {
				t.Fatalf("user %+v, turns %d, usage %+v, stdout %q", user, s.Turns(), c.usage, out.String())
			}
			if s.history[2].User.Text != "next question" || model.requests[1].History[1].Assistant.Blocks[0].Text != "Review complete." {
				t.Fatal("review history not carried to next turn")
			}
			paths, err := filepath.Glob(filepath.Join(c.traceDir, "*", "events.jsonl"))
			if err != nil || len(paths) != 2 {
				t.Fatalf("traces %v: %v", paths, err)
			}
			found := false
			for _, p := range paths {
				found = found || readEvents(t, p)[0].Data.(map[string]any)["prompt"] == want
			}
			if !found {
				t.Fatal("review prompt not traced")
			}
		})
	}
}

func TestChat_ReviewInvalidOrBlockedSpendsNothing(t *testing.T) {
	for _, argument := range []string{"unknown", "HIGH", "medium extra", "low\nextra"} {
		t.Run(argument, func(t *testing.T) {
			model := &compactModel{}
			c := newConversation(t, "gpt-6-luna", "low", 0)
			c.session.model = model
			var out, errs bytes.Buffer
			input := &fakeLineReader{reads: []struct {
				line string
				err  error
			}{{line: "/review " + argument}, {line: "/exit"}}}
			if code := chat(context.Background(), c, input, &out, &errs); code != exitOK || len(model.requests) != 0 || c.session.Turns() != 0 || !strings.Contains(errs.String(), "usage: /review [low|medium|high|max]") {
				t.Fatalf("requests %d stderr %s", len(model.requests), &errs)
			}
		})
	}
	c := newConversation(t, "gpt-6-luna", "low", 0)
	model := &compactModel{}
	c.session.model, c.session.blocked = model, string(StatusProtocolError)
	var out, errs bytes.Buffer
	chat(context.Background(), c, newLineReader(strings.NewReader("/review\n/exit\n"), &errs, nil), &out, &errs)
	if len(model.requests) != 0 || !strings.Contains(errs.String(), "the last run ended with protocol_error") {
		t.Fatalf("blocked review: %s", &errs)
	}
}

func TestChat_ReviewCompletionAndCompaction(t *testing.T) {
	c := newConversation(t, "gpt-6-luna", "low", 0)
	if !strings.Contains(chatHelp(), "/review") || !commandTakesArgument("/review") || !reflect.DeepEqual(c.completionArguments("/review"), []string{"low", "medium", "high", "max"}) {
		t.Fatal("review help/completion missing")
	}
	model := &compactModel{replies: []ModelResponse{turn(textBlock("handoff")), turn(textBlock("Review complete."))}}
	c = autoCompactConversation(t, model, 840_000)
	var out, errs bytes.Buffer
	chat(context.Background(), c, newLineReader(strings.NewReader("/review high\n/exit\n"), &errs, nil), &out, &errs)
	if len(model.requests) != 2 || model.requests[1].History[0].Kind != EntrySummary || !strings.Contains(model.requests[1].History[1].User.Text, "Review level: high") {
		t.Fatal("review bypassed ordinary compaction")
	}
}

func reviewSpawn(id, task, brief, authority string) OutputBlock {
	args, _ := json.Marshal(spawnArgs{Task: task, Authority: authority, Context: &agentContext{Brief: brief, Files: []string{"ratio.go"}}})
	return callBlock(id, "spawn", string(args))
}

func reviewToolResult(t *testing.T, history []Entry, id string) ToolOutcome {
	t.Helper()
	for _, entry := range history {
		if entry.Tool != nil && entry.Tool.CallID == id {
			if !entry.Tool.Outcome.OK {
				t.Fatalf("%s: %+v", id, entry.Tool.Outcome)
			}
			return entry.Tool.Outcome
		}
	}
	t.Fatalf("missing result %s", id)
	return ToolOutcome{}
}

// The scripts exercise orchestration and tool permissions, not a live model's
// willingness to follow the embedded protocol.
func TestChat_ReviewPipelineOffline(t *testing.T) {
	for _, level := range []string{"medium", "high", "max"} {
		t.Run(level, func(t *testing.T) {
			s := agentFixture(t)
			ws := s.cfg.Workspace
			root := ws.Root()
			gitInspectTestWrite(t, root, "go.mod", "module reviewfixture\n\ngo 1.23\n")
			gitInspectTestWrite(t, root, "ratio.go", "package reviewfixture\nfunc Ratio(n int) int { if n == 0 { return 0 }; return 10/n }\n")
			gitInspectTestGit(t, root, "init", "-q", "--template="+t.TempDir(), "-b", "main")
			gitInspectTestGit(t, root, "add", ".")
			gitInspectTestGit(t, root, "commit", "-q", "-m", "base")
			gitInspectTestGit(t, root, "checkout", "-q", "-b", "review")
			gitInspectTestWrite(t, root, "ratio.go", "package reviewfixture\nfunc Ratio(n int) int { return 10/n }\n")
			gitInspectTestGit(t, root, "add", "ratio.go")
			gitInspectTestGit(t, root, "commit", "-q", "-m", "remove guard")
			gitInspectTestWrite(t, root, "untracked.txt", "uncommitted evidence\n")
			tools := []Tool{NewGitInspectTool(ws)}
			for _, spec := range s.cfg.Registry.Specs() {
				tool, _ := s.cfg.Registry.Lookup(spec.Name)
				tools = append(tools, tool)
			}
			reg, err := NewRegistry(Mode{}, tools...)
			if err != nil {
				t.Fatal(err)
			}
			s.cfg.Registry = reg.bindWorkspace(s, ws)
			angles := []string{"Line by line", "Removed behavior", "Callers/callees", "Go pitfalls", "Feature off", "Realistic scale", "Claims against evidence", "Altitude", "Project conventions", "Executable probes"}
			candidates := []string{
				`{"file":"ratio.go","line":2,"summary":"zero divisor panics","scenario":"Ratio(0)"}`,
				`{"file":"ratio.go","line":2,"summary":"unexpected truncation","scenario":"Ratio(3) consumer expects precision"}`,
				`{"file":"ratio.go","line":2,"summary":"positive input panics","scenario":"Ratio(2)"}`,
			}
			verdicts := []string{
				"confirmed ratio.go:2 Ratio(0) panics",
				"plausible ratio.go:2 precision contract not established",
				"refuted ratio.go:2 quoted proof: `return 10/n`; n=2 is nonzero, result 5",
			}
			var mu sync.Mutex
			childSessions := map[string]string{}
			probeRan := false
			s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
				return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					task := req.History[0].User.Text
					respond := func(blocks ...OutputBlock) (ModelResponse, error) {
						r := turn(blocks...)
						r.Usage = Usage{Known: true, InputTokens: 2, OutputTokens: 1}
						return r, nil
					}
					if strings.HasPrefix(task, "prior outer") {
						if req.Scope.Step == 1 {
							return respond(spawnBlock("old-nested", "prior nested", ""))
						}
						return respond(textBlock("prior done"))
					}
					if strings.HasPrefix(task, "prior nested") {
						return respond(textBlock("prior nested done"))
					}
					if !strings.Contains(task, "Do not spawn or delegate.") {
						t.Error("review child lacks no-nesting rule")
					}
					mu.Lock()
					childSessions[req.Scope.SessionID] = task
					mu.Unlock()
					if strings.HasPrefix(task, "find ") {
						if !cfg.Registry.Mode().ReadOnly {
							t.Error("finder is not read-only")
						}
						if !strings.Contains(task, `\"Legibility beats features\"`) {
							t.Error("finder lacks quoted convention")
						}
						if req.Scope.Step == 1 {
							return respond(callBlock("source", "read_file", `{"path":"ratio.go"}`), callBlock("diff", "git_inspect", `{"operation":"diff","from":"main","to":"HEAD","paths":["ratio.go"]}`))
						}
						reviewToolResult(t, req.History, "source")
						reviewToolResult(t, req.History, "diff")
						for i := range candidates {
							if strings.HasPrefix(task, "find "+angles[i]+"\n") {
								return respond(textBlock(candidates[i] + "\nFINDER_REASONING private deliberations"))
							}
						}
						if strings.HasPrefix(task, "find Go pitfalls\n") {
							return respond(textBlock(candidates[0])) // Same failure, not another verifier.
						}
						return respond(textBlock("[]"))
					}
					if strings.HasPrefix(task, "verify ") {
						if strings.Contains(task, "AUTHOR_REASONING") || strings.Contains(task, "FINDER_REASONING") {
							t.Error("verifier contaminated by reasoning")
						}
						index := 0
						if strings.Contains(task, candidates[1]) {
							index = 1
						}
						if strings.Contains(task, candidates[2]) {
							index = 2
						}
						probe := level == "max" && index == 0
						if cfg.Registry.Mode().ReadOnly == probe {
							t.Error("wrong verifier authority")
						}
						if req.Scope.Step == 1 {
							return respond(callBlock("verify-source", "read_file", `{"path":"ratio.go"}`), callBlock("verify-diff", "git_inspect", `{"operation":"diff","from":"main","to":"HEAD","paths":["ratio.go"]}`))
						}
						if !probe {
							return respond(textBlock(verdicts[index]))
						}
						switch req.Scope.Step {
						case 2:
							return respond(callBlock("absence", "list_files", `{"path":"."}`))
						case 3:
							code := "package reviewfixture\nimport \"testing\"\nfunc TestReviewZero(t *testing.T) { defer func() { if recover() == nil { t.Fatal(\"expected zero-divisor panic\") }; t.Log(\"confirmed Ratio(0) panic\") }(); Ratio(0) }\n"
							args, _ := json.Marshal(map[string]string{"path": "zz_review_probe_zero_test.go", "content": code})
							return respond(callBlock("create", "write_file", string(args)))
						case 4:
							return respond(callBlock("probe", "exec", `{"argv":["go","test","-run","^TestReviewZero$","-v","."],"cwd":"."}`))
						case 5:
							outcome := reviewToolResult(t, req.History, "probe")
							if !strings.Contains(string(outcome.Data), "confirmed Ratio(0) panic") || !strings.Contains(string(outcome.Data), `"exit_code":0`) {
								t.Errorf("probe did not confirm: %s", outcome.Data)
							}
							mu.Lock()
							probeRan = true
							mu.Unlock()
							return respond(callBlock("read-probe", "read_file", `{"path":"zz_review_probe_zero_test.go"}`))
						case 6:
							return respond(callBlock("cleanup", "delete_file", `{"path":"zz_review_probe_zero_test.go"}`))
						default:
							reviewToolResult(t, req.History, "cleanup")
							return respond(textBlock(verdicts[index] + "; probe: go test -run ^TestReviewZero$ -v . exit 0; scratch removed"))
						}
					}
					if strings.HasPrefix(task, "sweep\n") {
						if !cfg.Registry.Mode().ReadOnly || strings.Contains(task, "AUTHOR_REASONING") {
							t.Error("sweep authority/context")
						}
						for _, verdict := range verdicts {
							if !strings.Contains(task, verdict) {
								t.Error("sweep lacks verified list")
							}
						}
						if !strings.Contains(task, "missing guards, defaults, setup/teardown") {
							t.Error("sweep gaps not requested")
						}
						return respond(textBlock("No additional candidates."))
					}
					t.Errorf("unexpected child task: %s", task)
					return respond(textBlock("unexpected"))
				}}, nil
			}
			if level == "max" {
				s.model = NewScriptedModel(turn(spawnBlock("prior", "prior outer", "")), turn(callBlock("prior-wait", "wait", `{"ids":["t1"]}`)), turn(textBlock("prior work")))
				agentTurn(t, s, "earlier unrelated task")
			}
			phase := 0
			nextID := 1
			if level == "max" {
				nextID = 3
			}
			baseline := agentSubtree{}
			verified := []string{}
			expectedReviewAgents := 13
			if level == "max" {
				expectedReviewAgents++
			}
			s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
				phase++
				switch phase {
				case 1:
					prompt := req.History[len(req.History)-1].User.Text
					for _, angle := range angles {
						if !strings.Contains(prompt, angle) {
							t.Errorf("embedded prompt lost angle %q", angle)
						}
					}
					for _, rule := range []string{"ONE new verifier Session", "NEVER include the author's reasoning", "Lean toward recall", "quoted counterevidence", "cannot later overrule", "zz_review_probe_*", "delete_file", "threads(subtree=true)", "next_offset", "baseline-to-final deltas", "If agents are disabled/unavailable"} {
						if !strings.Contains(prompt, rule) {
							t.Errorf("embedded prompt lost rule %q", rule)
						}
					}
					if !strings.Contains(prompt, "Review level: "+level) {
						t.Error("command level lost")
					}
					return turn(callBlock("baseline", "threads", `{"subtree":true}`), callBlock("inventory", "list_files", `{"path":"."}`), callBlock("uncommitted", "read_file", `{"path":"untracked.txt"}`)), nil
				case 2:
					data(t, reviewToolResult(t, req.History, "baseline"), &baseline)
					if !strings.Contains(string(reviewToolResult(t, req.History, "uncommitted").Data), "uncommitted evidence") {
						t.Error("uncommitted file not inspected")
					}
					var batch []OutputBlock
					for i := 0; i < 8; i++ {
						batch = append(batch, reviewSpawn(fmt.Sprintf("finder-%d", i), "find "+angles[i], "Do not spawn or delegate. main vs HEAD; ratio.go:2; AGENTS: \"Legibility beats features\". Independent angle.", "read"))
					}
					return turn(batch...), nil
				case 3:
					ids := []string{}
					for i := 0; i < 8; i++ {
						ids = append(ids, fmt.Sprintf("t%d", nextID+i))
					}
					raw, _ := json.Marshal(map[string]any{"ids": ids})
					return turn(callBlock("find-wait", "wait", string(raw))), nil
				case 4:
					var batch []OutputBlock
					for i := 8; i < 10; i++ {
						batch = append(batch, reviewSpawn(fmt.Sprintf("finder-%d", i), "find "+angles[i], "Do not spawn or delegate. main vs HEAD; ratio.go:2; AGENTS: \"Legibility beats features\".", "read"))
					}
					return turn(batch...), nil
				case 5:
					raw, _ := json.Marshal(map[string]any{"ids": []string{fmt.Sprintf("t%d", nextID+8), fmt.Sprintf("t%d", nextID+9)}})
					return turn(callBlock("remaining-wait", "wait", string(raw))), nil
				case 6:
					delivered := agentResults(req.History)
					var batch []OutputBlock
					for i, candidate := range candidates {
						found := false
						for _, result := range delivered {
							found = found || strings.Contains(result.Result.Reply, candidate)
						}
						if !found {
							t.Errorf("candidate not delivered: %s", candidate)
						}
						authority := "read"
						if level == "max" && i == 0 {
							authority = "write"
						}
						batch = append(batch, reviewSpawn(fmt.Sprintf("verifier-%d", i), "verify "+candidate, "Do not spawn or delegate. Independently inspect main vs HEAD and ratio.go. confirmed/plausible/refuted; refuted requires quoted proof. No author reasoning. AGENTS: \"Legibility beats features\".", authority))
					}
					return turn(batch...), nil
				case 7:
					raw, _ := json.Marshal(map[string]any{"ids": []string{fmt.Sprintf("t%d", nextID+10), fmt.Sprintf("t%d", nextID+11), fmt.Sprintf("t%d", nextID+12)}})
					return turn(callBlock("verify-wait", "wait", string(raw))), nil
				case 8:
					for _, result := range agentResults(req.History) {
						for _, verdict := range verdicts {
							if strings.HasPrefix(result.Result.Reply, verdict) {
								verified = append(verified, result.Result.Reply)
							}
						}
					}
					if len(verified) != 3 {
						t.Fatalf("verified list %v", verified)
					}
					if level == "max" {
						return turn(reviewSpawn("sweep", "sweep", "Do not spawn or delegate. main vs HEAD; missing guards, defaults, setup/teardown. Verified list:\n"+strings.Join(verified, "\n"), "read")), nil
					}
					return turn(callBlock("final-tree", "threads", `{"subtree":true}`)), nil
				case 9:
					if level == "max" {
						raw, _ := json.Marshal(map[string]any{"ids": []string{fmt.Sprintf("t%d", nextID+13)}})
						return turn(callBlock("sweep-wait", "wait", string(raw))), nil
					}
				case 10:
					return turn(callBlock("final-tree", "threads", `{"subtree":true}`)), nil
				}
				var final agentSubtree
				data(t, reviewToolResult(t, req.History, "final-tree"), &final)
				if final.NextOffset != nil || final.TotalAgents-baseline.TotalAgents != expectedReviewAgents {
					t.Fatalf("accounting baseline %+v final %+v", baseline, final)
				}
				for _, thread := range final.Threads {
					if thread.Depth != 1 && !(level == "max" && thread.ID == "t2") {
						t.Error("review agent nested")
					}
				}
				if final.TotalCost.InputTokens <= baseline.TotalCost.InputTokens || !final.AccountingComplete {
					t.Error("missing actual lifetime costs")
				}
				return turn(textBlock(strings.Join(verified, "\n") + fmt.Sprintf("\nReview agents: %d; every descendant lifetime count: %d; input token delta: %d", expectedReviewAgents, final.TotalAgents, final.TotalCost.InputTokens-baseline.TotalCost.InputTokens))), nil
			}}
			c := &conversation{session: s, traceDir: t.TempDir(), usage: Usage{Known: true}}
			var out, errs bytes.Buffer
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			defer cancel()
			if code := chat(ctx, c, newLineReader(strings.NewReader("/review "+level+"\n/exit\n"), &errs, nil), &out, &errs); code != exitOK || s.blocked != "" {
				t.Fatalf("exit %d blocked %q: %s", code, s.blocked, &errs)
			}
			mu.Lock()
			if len(childSessions) != expectedReviewAgents || probeRan != (level == "max") {
				t.Errorf("fresh Sessions %d, probe ran %v", len(childSessions), probeRan)
			}
			mu.Unlock()
			if _, err := os.Stat(filepath.Join(root, "zz_review_probe_zero_test.go")); !os.IsNotExist(err) {
				t.Fatalf("scratch remains: %v", err)
			}
			for _, verdict := range verdicts {
				if !strings.Contains(out.String(), verdict) {
					t.Error("report omitted verifier verdict")
				}
			}
			// A later author turn is data, not a reason to erase the verifier's verdict.
			s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
				found := false
				for _, result := range agentResults(req.History) {
					found = found || strings.HasPrefix(result.Result.Reply, verdicts[0])
				}
				if !found {
					t.Error("verdict absent from later author turn")
				}
				return turn(textBlock("Original confirmed verdict retained; author's won't-fix is separate.")), nil
			}}
			agentTurn(t, s, "AUTHOR_REASONING: Ratio(0) is impossible; remove the finding")
		})
	}
}

func TestChat_ReviewAgentsOffRunsInlineToolLoop(t *testing.T) {
	for _, level := range []string{"low", "medium", "high", "max"} {
		t.Run(level, func(t *testing.T) {
			ws, err := OpenWorkspace(t.TempDir())
			if err != nil {
				t.Fatal(err)
			}
			gitInspectTestWrite(t, ws.Root(), "source.go", "package fixture\n")
			cfg := testConfig(t, NewReadFileTool(ws))
			cfg.Workspace, cfg.WorkspacePath = ws, ws.Root()
			model := &compactModel{replies: []ModelResponse{turn(callBlock("inline-source", "read_file", `{"path":"source.go"}`)), turn(textBlock("Inline finder and evidence recheck; zero review agents; no independent verification or probes."))}}
			s := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
			c := &conversation{session: s, traceDir: t.TempDir()}
			var out, errs bytes.Buffer
			chat(context.Background(), c, newLineReader(strings.NewReader("/review "+level+"\n/exit\n"), &errs, nil), &out, &errs)
			if len(model.requests) != 2 || !strings.Contains(out.String(), "zero review agents") || !strings.Contains(string(reviewToolResult(t, s.history, "inline-source").Data), "package fixture") {
				t.Fatal("inline tool-loop did not inspect source")
			}
			for _, spec := range model.requests[0].Tools {
				if spec.Name == "spawn" {
					t.Fatal("agents off exposed spawn")
				}
			}
		})
	}
}

func TestChat_ReviewProbePermissionLimits(t *testing.T) {
	for _, mode := range []string{"read-only", "plan"} {
		t.Run(mode, func(t *testing.T) {
			ws, err := OpenWorkspace(t.TempDir())
			if err != nil {
				t.Fatal(err)
			}
			registry, err := NewRegistry(Mode{ReadOnly: mode == "read-only"}, NewWriteFileTool(ws), NewExecTool(ws))
			if err != nil {
				t.Fatal(err)
			}
			model := &compactModel{replies: []ModelResponse{
				turn(callBlock("probe-write", "write_file", `{"path":"zz_review_probe_denied.txt","content":"probe"}`), callBlock("probe-exec", "exec", `{"argv":["go","version"],"cwd":"."}`)),
				turn(textBlock("Probe skipped: permissions unavailable; source-only review.")),
			}}
			cfg := testConfig(t)
			cfg.Registry, cfg.Workspace, cfg.WorkspacePath, cfg.PlanMode = registry, ws, ws.Root(), mode == "plan"
			s := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
			c := &conversation{session: s, traceDir: t.TempDir()}
			var out, errs bytes.Buffer
			chat(context.Background(), c, newLineReader(strings.NewReader("/review max\n/exit\n"), &errs, nil), &out, &errs)
			if len(model.requests) != 2 || !strings.Contains(out.String(), "Probe skipped") {
				t.Fatalf("requests %d: %s", len(model.requests), &errs)
			}
			for _, result := range results(s) {
				if result.Outcome.OK || result.Outcome.Effect != EffectNone {
					t.Errorf("probe escaped %s: %+v", mode, result)
				}
			}
			if _, err := os.Stat(filepath.Join(ws.Root(), "zz_review_probe_denied.txt")); !os.IsNotExist(err) {
				t.Fatalf("probe wrote under %s: %v", mode, err)
			}
		})
	}
}
