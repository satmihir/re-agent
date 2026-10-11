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

// reviewSpawn preserves legacy anonymous-dispatch fixtures without paging.
// The named-finder recovery test exercises the current retention protocol.
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

// This legacy anonymous-dispatch fixture exercises orchestration and permissions,
// not the current named-page protocol or a live model's willingness to follow it.
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
			sweepCandidate := `{"file":"ratio.go","line":2,"summary":"negative inputs lack validation","scenario":"Ratio(-1) enters an unsupported domain"}`
			sweepVerdict := "plausible ratio.go:2 negative input validation contract not established; source-only, no executable domain oracle"
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
						cap := map[string]string{"medium": "N=5", "high": "N=10", "max": "N=20"}[level]
						if !strings.Contains(task, cap) {
							t.Error("finder lacks level candidate cap")
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
						verdict := verdicts[index]
						if strings.Contains(task, sweepCandidate) {
							index, verdict = 3, sweepVerdict
						}
						probe := level == "max" && index == 0
						if cfg.Registry.Mode().ReadOnly == probe {
							t.Error("wrong verifier authority")
						}
						if req.Scope.Step == 1 {
							return respond(callBlock("verify-source", "read_file", `{"path":"ratio.go"}`), callBlock("verify-diff", "git_inspect", `{"operation":"diff","from":"main","to":"HEAD","paths":["ratio.go"]}`))
						}
						if !probe {
							return respond(textBlock(verdict))
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
							return respond(textBlock(verdict + "; probe: go test -run ^TestReviewZero$ -v . exit 0; scratch removed"))
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
						return respond(textBlock(sweepCandidate))
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
				expectedReviewAgents += 2
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
					for _, rule := range []string{"ONE new verifier Session", "NEVER include the author's reasoning", "Lean toward recall", "quoted counterevidence", "cannot later overrule", "zz_review_probe_*", "delete_file", "threads(subtree=true)", "next_offset", "baseline-to-final deltas", "If agents are disabled/unavailable", "low N=3 K=3", "medium N=5 K=5", "high N=10 K=10", "max N=20 K=20", "correctness failures before cleanup/style", "retain ALL overflow candidate records", "supplied PR description", "final-report claims", "actual test output and the diff", "nothing else changed"} {
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
						batch = append(batch, reviewSpawn(fmt.Sprintf("finder-%d", i), "find "+angles[i], "Do not spawn or delegate. main vs HEAD; ratio.go:2; AGENTS: \"Legibility beats features\". Independent angle. Candidate cap "+map[string]string{"medium": "N=5", "high": "N=10", "max": "N=20"}[level], "read"))
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
						batch = append(batch, reviewSpawn(fmt.Sprintf("finder-%d", i), "find "+angles[i], "Do not spawn or delegate. main vs HEAD; ratio.go:2; AGENTS: \"Legibility beats features\". Candidate cap "+map[string]string{"medium": "N=5", "high": "N=10", "max": "N=20"}[level], "read"))
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
					found := false
					for _, result := range agentResults(req.History) {
						found = found || result.Result.Reply == sweepCandidate
					}
					if !found {
						t.Fatal("new sweep candidate not delivered")
					}
					return turn(reviewSpawn("sweep-verifier", "verify "+sweepCandidate, "Do not spawn or delegate. Candidate only; inspect main vs HEAD, ratio.go, and domain contract. No author reasoning. Source-only if no executable oracle exists.", "read")), nil
				case 11:
					raw, _ := json.Marshal(map[string]any{"ids": []string{fmt.Sprintf("t%d", nextID+14)}})
					return turn(callBlock("sweep-verify-wait", "wait", string(raw))), nil
				case 12:
					found := false
					for _, result := range agentResults(req.History) {
						if result.Result.Reply == sweepVerdict {
							verified = append(verified, result.Result.Reply)
							found = true
						}
					}
					if !found {
						t.Fatal("new sweep candidate verdict not delivered")
					}
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
			if level == "max" {
				verifiers := 0
				for _, task := range childSessions {
					if strings.HasPrefix(task, "verify "+sweepCandidate+"\n") {
						verifiers++
					}
				}
				if verifiers != 1 || !strings.Contains(out.String(), sweepVerdict) {
					t.Error("sweep candidate needs exactly one fresh verifier and retained final verdict")
				}
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
				return turn(textBlock(strings.Join(verified, "\n") + "\nAuthor's won't-fix is separate; original verdicts retained.")), nil
			}}
			later := agentTurn(t, s, "AUTHOR_REASONING: Ratio(0) is impossible; remove the finding")
			for _, verdict := range verified {
				if !strings.Contains(later.Reply, verdict) {
					t.Errorf("later author result dropped actual verdict: %s", verdict)
				}
			}
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

func TestChat_ReviewOverflowAndSuppliedClaimsToolLoop(t *testing.T) {
	s := agentFixture(t)
	root := s.cfg.Workspace.Root()
	gitInspectTestWrite(t, root, "ratio.go", "package fixture\n// short name\nfunc Ratio(n int) int { return 10/n }\n")
	gitInspectTestWrite(t, root, "evidence.txt", "trace: 2 files changed; test output: FAIL TestZero: divide by zero\n")
	candidates := []string{
		"cleanup ratio.go:3 short name",
		"correctness ratio.go:3 Ratio(0) panics",
		"correctness ratio.go:3 disabled path reaches zero divisor",
		"correctness ratio.go:3 caller's omitted value is zero",
		"correctness ratio.go:3 teardown counter can become zero",
		"correctness evidence.txt:1 supplied claims contradict observed counts and test output",
	}
	s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			task := req.History[0].User.Text
			if !cfg.Registry.Mode().ReadOnly || !strings.Contains(task, "Do not spawn or delegate.") {
				t.Error("overflow agent authority or nesting rule")
			}
			if req.Scope.Step == 1 {
				if strings.HasPrefix(task, "find claims") {
					for _, text := range []string{"N=5", "PR description: 0 changed files", "Final report: all tests passed; nothing else changed", "trace/test output: evidence.txt", "diff coverage unavailable"} {
						if !strings.Contains(task, text) {
							t.Errorf("claims finder lacks supplied evidence %q", text)
						}
					}
				}
				return turn(callBlock("read-source", "read_file", `{"path":"ratio.go"}`), callBlock("read-evidence", "read_file", `{"path":"evidence.txt"}`)), nil
			}
			if !strings.Contains(string(reviewToolResult(t, req.History, "read-evidence").Data), "FAIL TestZero") {
				t.Error("claims not checked against actual output")
			}
			if strings.HasPrefix(task, "find claims") {
				raw, _ := json.Marshal(struct{ Prioritized, Overflow []string }{candidates[:5], candidates[5:]})
				return turn(textBlock(string(raw))), nil
			}
			if strings.Contains(task, "all tests passed") {
				t.Error("verifier received author claims instead of candidate-only brief")
			}
			for _, candidate := range candidates {
				if strings.HasPrefix(task, "verify "+candidate+"\n") {
					return turn(textBlock("plausible " + candidate)), nil
				}
			}
			t.Error("unexpected overflow verifier")
			return turn(textBlock("unexpected")), nil
		}}, nil
	}
	step := 0
	s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
		step++
		switch step {
		case 1:
			return turn(reviewSpawn("claims", "find claims", "Do not spawn or delegate. N=5; retain overflow. PR description: 0 changed files. Final report: all tests passed; nothing else changed. trace/test output: evidence.txt; diff coverage unavailable, disclose it.", "read")), nil
		case 2:
			return turn(callBlock("finder-wait", "wait", `{"ids":["t1"]}`)), nil
		case 3:
			var records struct{ Prioritized, Overflow []string }
			for _, result := range agentResults(req.History) {
				if result.ID == "t1" {
					if err := json.Unmarshal([]byte(result.Result.Reply), &records); err != nil {
						t.Fatal(err)
					}
				}
			}
			if len(records.Prioritized) != 5 || len(records.Overflow) != 1 {
				t.Fatalf("finder caps or overflow lost: %+v", records)
			}
			var batch []OutputBlock
			for i, candidate := range append(records.Prioritized, records.Overflow...) {
				batch = append(batch, reviewSpawn(fmt.Sprintf("verify-%d", i), "verify "+candidate, "Do not spawn or delegate. Candidate only; ratio.go and evidence.txt. Independently recheck; uncertainty is plausible.", "read"))
			}
			return turn(batch...), nil
		case 4:
			return turn(callBlock("all-verifiers", "wait", `{"ids":["t2","t3","t4","t5","t6","t7"]}`)), nil
		}
		var correctness, cleanup []string
		for _, result := range agentResults(req.History) {
			if strings.HasPrefix(result.Result.Reply, "plausible correctness") {
				correctness = append(correctness, result.Result.Reply)
			}
			if strings.HasPrefix(result.Result.Reply, "plausible cleanup") {
				cleanup = append(cleanup, result.Result.Reply)
			}
		}
		if len(correctness) != 5 || len(cleanup) != 1 {
			t.Fatalf("overflow not freshly verified: %v %v", correctness, cleanup)
		}
		return turn(textBlock("Ranked top K=5, correctness before cleanup:\n" + strings.Join(correctness, "\n") + "\nOverflow appendix: 1 finding retained\n" + strings.Join(cleanup, "\n") + "\nClaims checked against observed trace/test output; diff coverage unavailable.")), nil
	}}
	c := &conversation{session: s, traceDir: t.TempDir()}
	var out, errs bytes.Buffer
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	chat(ctx, c, newLineReader(strings.NewReader("/review medium\n/exit\n"), &errs, nil), &out, &errs)
	if s.blocked != "" || !strings.Contains(out.String(), "Overflow appendix: 1 finding retained") || strings.Index(out.String(), "plausible correctness") > strings.Index(out.String(), "plausible cleanup") {
		t.Fatalf("ranked capped report: %s stderr: %s", &out, &errs)
	}
	for _, candidate := range candidates {
		if !strings.Contains(out.String(), candidate) {
			t.Errorf("candidate lost at cap: %s", candidate)
		}
	}
	if s.agentSubtree().TotalAgents != 7 {
		t.Fatal("overflow candidate did not get a fresh verifier")
	}
}

// Legacy anonymous dispatch keeps this fixture focused on shard size/coverage;
// named retention and dismissal are covered by the recovery tool-loop test.
func TestChat_ReviewShardScalingFromObservedDiff(t *testing.T) {
	for _, tc := range []struct {
		name, level          string
		files, lines, shards int
	}{
		{"small-boundary", "high", 5, 300, 1},
		{"file-crosses-small", "high", 6, 300, 2},
		{"line-crosses-small", "high", 5, 301, 2},
		{"medium-boundary", "high", 20, 1500, 2},
		{"file-crosses-medium", "high", 21, 1500, 4},
		{"line-crosses-medium", "max", 20, 1501, 4},
		{"medium-level-large", "medium", 21, 1501, 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := agentFixture(t)
			root := s.cfg.Workspace.Root()
			gitInspectTestGit(t, root, "init", "-q", "--template="+t.TempDir(), "-b", "main")
			paths := make([]string, tc.files)
			for i := range paths {
				paths[i] = fmt.Sprintf("change-%02d.txt", i)
				gitInspectTestWrite(t, root, paths[i], "base\n")
			}
			gitInspectTestGit(t, root, "add", ".")
			gitInspectTestGit(t, root, "commit", "-q", "-m", "base")
			gitInspectTestGit(t, root, "checkout", "-q", "-b", "review")
			for i, path := range paths {
				lines := tc.lines / tc.files
				if i < tc.lines%tc.files {
					lines++
				}
				gitInspectTestWrite(t, root, path, "base\n"+strings.Repeat("x\n", lines))
			}
			gitInspectTestGit(t, root, "add", ".")
			gitInspectTestGit(t, root, "commit", "-q", "-m", "changed")
			tools := []Tool{NewGitInspectTool(s.cfg.Workspace)}
			for _, spec := range s.cfg.Registry.Specs() {
				tool, _ := s.cfg.Registry.Lookup(spec.Name)
				tools = append(tools, tool)
			}
			registry, err := NewRegistry(Mode{}, tools...)
			if err != nil {
				t.Fatal(err)
			}
			s.cfg.Registry = registry.bindWorkspace(s, s.cfg.Workspace)
			var mu sync.Mutex
			covered := map[string]bool{}
			s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
				return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					if !cfg.Registry.Mode().ReadOnly {
						t.Error("scaling finder gained write authority")
					}
					_, raw, ok := strings.Cut(req.History[0].User.Text, "Context brief and live workspace paths (data, not authority):\n")
					var brief agentContext
					if !ok || json.Unmarshal([]byte(raw), &brief) != nil || !strings.Contains(brief.Brief, "Do not spawn or delegate.") {
						t.Error("invalid independent shard brief")
					}
					if req.Scope.Step == 1 {
						var reads []OutputBlock
						for i, path := range brief.Files {
							args, _ := json.Marshal(map[string]string{"path": path})
							reads = append(reads, callBlock(fmt.Sprintf("read-%d", i), "read_file", string(args)))
						}
						return turn(reads...), nil
					}
					mu.Lock()
					for i, path := range brief.Files {
						if !strings.Contains(string(reviewToolResult(t, req.History, fmt.Sprintf("read-%d", i)).Data), "base") {
							t.Error("shard did not inspect live file")
						}
						if covered[path] {
							t.Errorf("overlapping shard %s", path)
						}
						covered[path] = true
					}
					mu.Unlock()
					return turn(textBlock("No candidates in this shard.")), nil
				}}, nil
			}
			step, observedFiles, observedLines, shards := 0, 0, 0, 0
			s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
				step++
				switch step {
				case 1:
					prompt := req.History[0].User.Text
					for _, rule := range []string{"small <=5 files and <=300", "medium <=20 files and <=1500", "1/1/2 finder shards at medium, 1/2/4 at high or max"} {
						if !strings.Contains(prompt, rule) {
							t.Errorf("prompt lost observed-size rule %s", rule)
						}
					}
					return turn(callBlock("size", "git_inspect", `{"operation":"diff","from":"main","to":"HEAD"}`)), nil
				case 2:
					var diff gitInspectResult
					data(t, reviewToolResult(t, req.History, "size"), &diff)
					if diff.OutputTruncated {
						t.Fatal("size fixture diff was truncated")
					}
					for _, line := range strings.Split(diff.Output, "\n") {
						if strings.HasPrefix(line, "diff --git ") {
							observedFiles++
						}
						if (strings.HasPrefix(line, "+") && !strings.HasPrefix(line, "+++")) || (strings.HasPrefix(line, "-") && !strings.HasPrefix(line, "---")) {
							observedLines++
						}
					}
					shards = 1
					if observedFiles > 5 || observedLines > 300 {
						if tc.level != "medium" {
							shards = 2
						}
					}
					if observedFiles > 20 || observedLines > 1500 {
						shards = 2
						if tc.level != "medium" {
							shards = 4
						}
					}
					var batch []OutputBlock
					for shard := 0; shard < shards; shard++ {
						var files []string
						for i, path := range paths {
							if i%shards == shard {
								files = append(files, path)
							}
						}
						args, _ := json.Marshal(spawnArgs{Task: fmt.Sprintf("find line-by-line shard %d", shard), Authority: "read", Context: &agentContext{Brief: fmt.Sprintf("Do not spawn or delegate. Observed %d files, %d lines; main vs HEAD; shard %d/%d.", observedFiles, observedLines, shard+1, shards), Files: files}})
						batch = append(batch, callBlock(fmt.Sprintf("shard-%d", shard), "spawn", string(args)))
					}
					return turn(batch...), nil
				case 3:
					var ids []string
					for i := 1; i <= shards; i++ {
						ids = append(ids, fmt.Sprintf("t%d", i))
					}
					raw, _ := json.Marshal(map[string]any{"ids": ids})
					return turn(callBlock("shard-wait", "wait", string(raw))), nil
				}
				return turn(textBlock(fmt.Sprintf("Observed %d files/%d lines; %d independent shards for this angle; other angles not exercised by this fixture.", observedFiles, observedLines, shards))), nil
			}}
			c := &conversation{session: s, traceDir: t.TempDir()}
			var out, errs bytes.Buffer
			ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
			defer cancel()
			chat(ctx, c, newLineReader(strings.NewReader("/review "+tc.level+"\n/exit\n"), &errs, nil), &out, &errs)
			if s.blocked != "" || observedFiles != tc.files || observedLines != tc.lines || shards != tc.shards || s.agentSubtree().TotalAgents != tc.shards {
				t.Fatalf("observed %d/%d shards %d: %s %s", observedFiles, observedLines, shards, &out, &errs)
			}
			mu.Lock()
			if len(covered) != tc.files {
				t.Errorf("shards covered %d/%d files", len(covered), tc.files)
			}
			mu.Unlock()
		})
	}
}

func TestChat_ReviewPagedSubtreeAccounting(t *testing.T) {
	s := agentFixture(t)
	for i := 0; i < 300; i++ {
		id := fmt.Sprintf("prior-%03d", i)
		s.agents.archive[id] = agentObservation{AgentThread: AgentThread{ID: id, ParentID: "root", State: "delivered", Task: strings.Repeat("prior review task ", 12), Cost: Usage{Known: true, InputTokens: 1}}, Depth: 1}
	}
	s.agentModel = func(Config, *Trace) (Model, error) {
		reply := turn(textBlock("Inline candidate verification not needed: no candidates."))
		reply.Usage = Usage{Known: true, InputTokens: 7, OutputTokens: 3}
		return NewScriptedModel(reply), nil
	}
	phase, offset, pageIndex := "baseline", 0, 0
	pageCall := ""
	var baseline, final agentSubtree
	seen := map[string]bool{}
	baselinePages, finalPages := 0, 0
	s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
		requestPage := func() (ModelResponse, error) {
			pageCall = fmt.Sprintf("%s-page-%d", phase, pageIndex)
			raw, _ := json.Marshal(map[string]any{"subtree": true, "offset": offset})
			return turn(callBlock(pageCall, "threads", string(raw))), nil
		}
		if phase == "wait" {
			phase = "after-wait"
			return turn(callBlock("finder-wait", "wait", `{"ids":["t1"]}`)), nil
		}
		if phase == "after-wait" {
			phase, offset, pageIndex, pageCall = "final", 0, 0, ""
			seen = map[string]bool{}
			return requestPage()
		}
		if pageCall == "" {
			return requestPage()
		}
		var page agentSubtree
		data(t, reviewToolResult(t, req.History, pageCall), &page)
		for _, thread := range page.Threads {
			if seen[thread.ID] {
				t.Errorf("duplicate paged agent %s", thread.ID)
			}
			seen[thread.ID] = true
		}
		if phase == "baseline" {
			baselinePages++
			if pageIndex == 0 {
				baseline = page
			} else if page.TotalAgents != baseline.TotalAgents || page.TotalCost != baseline.TotalCost {
				t.Error("baseline totals changed across pages")
			}
		} else {
			finalPages++
			if pageIndex == 0 {
				final = page
			} else if page.TotalAgents != final.TotalAgents || page.TotalCost != final.TotalCost {
				t.Error("final totals changed across pages")
			}
		}
		if page.NextOffset != nil {
			if *page.NextOffset <= offset {
				t.Fatal("subtree paging did not advance")
			}
			offset = *page.NextOffset
			pageIndex++
			return requestPage()
		}
		if len(seen) != page.TotalAgents {
			t.Fatalf("paged view saw %d/%d descendants", len(seen), page.TotalAgents)
		}
		if phase == "baseline" {
			phase = "wait"
			return turn(reviewSpawn("finder", "find evidence", "Do not spawn or delegate. Read-only evidence review; no candidates expected.", "read")), nil
		}
		return turn(textBlock(fmt.Sprintf("Every descendant: %d; baseline %d; review agents delta %d; input token delta %d; output token delta %d; paged IDs %d. Totals counted once, not once per page.", final.TotalAgents, baseline.TotalAgents, final.TotalAgents-baseline.TotalAgents, final.TotalCost.InputTokens-baseline.TotalCost.InputTokens, final.TotalCost.OutputTokens-baseline.TotalCost.OutputTokens, len(seen)))), nil
	}}
	c := &conversation{session: s, traceDir: t.TempDir()}
	var out, errs bytes.Buffer
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	chat(ctx, c, newLineReader(strings.NewReader("/review medium\n/exit\n"), &errs, nil), &out, &errs)
	if baselinePages < 2 || finalPages < 2 || s.blocked != "" {
		t.Fatalf("pages %d/%d blocked %s: %s", baselinePages, finalPages, s.blocked, &errs)
	}
	for _, want := range []string{"Every descendant: 301", "baseline 300", "review agents delta 1", "input token delta 7", "output token delta 3", "paged IDs 301"} {
		if !strings.Contains(out.String(), want) {
			t.Errorf("paged accounting report lacks %q: %s", want, &out)
		}
	}
}

func TestChat_ReviewNamedFinderRecoversTruncatedDeliveryWithoutTrace(t *testing.T) {
	s := agentFixture(t)
	type record struct {
		ID, File, Summary string
		Line              int
	}
	type page struct {
		Records       []record
		Total, Cursor int
		Next          *int
	}
	var records []record
	for i := 0; i < 24; i++ {
		records = append(records, record{ID: fmt.Sprintf("candidate-%02d", i), File: "ratio.go", Line: i + 1, Summary: strings.Repeat("failure scenario ", 90)})
	}
	full, _ := json.Marshal(page{Records: records, Total: len(records)})
	if len(full) <= MaxResultBytes {
		t.Fatal("fixture must exceed delivery cap")
	}
	followups := 0
	s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			if !cfg.Registry.Mode().ReadOnly {
				t.Error("recovery finder gained writes")
			}
			if len(req.History) == 1 {
				return turn(textBlock(string(full))), nil
			}
			// Recover from the complete earlier assistant output in this same child,
			// never from the parent's truncated delivery or an external trace file.
			var retained page
			if err := json.Unmarshal([]byte(req.History[1].Assistant.Blocks[0].Text), &retained); err != nil || len(retained.Records) != 24 {
				t.Errorf("child lost full retained output: %v", err)
			}
			var request struct{ Cursor, Limit int }
			if err := json.Unmarshal([]byte(req.History[len(req.History)-1].User.Text), &request); err != nil {
				t.Error(err)
			}
			if request.Limit != 3 {
				t.Error("parent did not request smaller bounded page")
			}
			end := min(request.Cursor+request.Limit, len(retained.Records))
			response := page{Records: retained.Records[request.Cursor:end], Total: retained.Total, Cursor: request.Cursor}
			if end < len(retained.Records) {
				response.Next = &end
			}
			raw, _ := json.Marshal(response)
			if len(raw) > 8192 {
				t.Error("recovery page exceeds encoded bound")
			}
			for _, r := range response.Records {
				rawRecord, _ := json.Marshal(r)
				if len(rawRecord) > 2048 {
					t.Error("record exceeds encoded bound")
				}
			}
			followups++
			return turn(textBlock(string(raw))), nil
		}}, nil
	}
	phase, cursor, callIndex := "spawn", 0, 0
	seen := map[string]record{}
	truncated := false
	s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
		callIndex++
		id := fmt.Sprintf("recovery-%d", callIndex)
		switch phase {
		case "spawn":
			phase = "wait"
			for _, rule := range []string{"JSON-encoded payload is <=8 KiB", "record <=2 KiB", "Use unique named review agents", "Dismiss only then", "not reconstruction from the truncated parent copy", "Do not claim missing candidates were retained or verified", "reuse a verifier for another candidate; page retrieval alone", "is permitted"} {
				if !strings.Contains(req.History[0].User.Text, rule) {
					t.Errorf("prompt lacks recovery rule %s", rule)
				}
			}
			args, _ := json.Marshal(spawnArgs{Name: "finder-pages", Task: "find candidates; Do not spawn or delegate.", Authority: "read", Context: &agentContext{Brief: "Retain records; stable IDs/count/cursor. Pages <=8 KiB, records <=2 KiB."}})
			return turn(callBlock(id, "spawn", string(args))), nil
		case "wait":
			phase = "collect"
			return turn(callBlock(id, "wait", `{"ids":["finder-pages"]}`)), nil
		case "report":
			return turn(textBlock(fmt.Sprintf("Recovered %d/24 candidate records via named Session pages; initial delivery truncated; zero trace reads; missing records 0.", len(seen)))), nil
		}
		all := agentResults(req.History)
		latest := all[len(all)-1]
		if !truncated {
			if !latest.Truncated || len(latest.Result.Reply) >= len(full) {
				t.Fatal("oversized initial delivery did not truncate")
			}
			truncated = true
		} else {
			if latest.Truncated {
				t.Fatal("bounded recovery result truncated")
			}
			var recovered page
			if err := json.Unmarshal([]byte(latest.Result.Reply), &recovered); err != nil {
				t.Fatal(err)
			}
			if recovered.Cursor != cursor || recovered.Total != 24 {
				t.Fatal("recovery cursor/count mismatch")
			}
			for _, r := range recovered.Records {
				if _, exists := seen[r.ID]; exists {
					t.Fatal("duplicate recovered record")
				}
				seen[r.ID] = r
			}
			if recovered.Next == nil {
				if len(seen) != 24 {
					t.Fatal("agent dismissed before all records collected")
				}
				phase = "report"
				return turn(callBlock(id, "dismiss", `{"name":"finder-pages"}`)), nil
			}
			cursor = *recovered.Next
		}
		phase = "wait"
		message, _ := json.Marshal(map[string]int{"Cursor": cursor, "Limit": 3})
		args, _ := json.Marshal(map[string]string{"name": "finder-pages", "message": string(message)})
		return turn(callBlock(id, "send", string(args))), nil
	}}
	var out, errs bytes.Buffer
	c := &conversation{session: s, traceDir: t.TempDir()}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	chat(ctx, c, newLineReader(strings.NewReader("/review medium\n/exit\n"), &errs, nil), &out, &errs)
	if s.blocked != "" || !truncated || followups != 8 || len(seen) != 24 || !strings.Contains(out.String(), "Recovered 24/24") {
		t.Fatalf("truncated %v followups %d records %d blocked %s: %s %s", truncated, followups, len(seen), s.blocked, &out, &errs)
	}
	for _, r := range records {
		if !reflect.DeepEqual(seen[r.ID], r) {
			t.Errorf("recovered candidate data differs: %s", r.ID)
		}
	}
	for _, result := range results(s) {
		if result.Name != "spawn" && result.Name != "wait" && result.Name != "send" && result.Name != "dismiss" {
			t.Errorf("unexpected trace recovery tool %s", result.Name)
		}
		if !result.Outcome.OK {
			t.Errorf("recovery tool failed: %+v", result)
		}
	}
}

func TestChat_ReviewFailedProbeStillReadsAndDeletesScratch(t *testing.T) {
	s := agentFixture(t)
	const scratch = "zz_review_probe_failed.txt"
	model := &compactModel{replies: []ModelResponse{
		turn(callBlock("absence", "list_files", `{"path":"."}`)),
		turn(callBlock("create", "write_file", `{"path":"zz_review_probe_failed.txt","content":"offline failed probe evidence\n"}`)),
		turn(callBlock("failed-probe", "exec", `{"argv":["sh","-c","printf 'probe assertion failed\\n'; exit 7"],"cwd":"."}`)),
		turn(callBlock("read-failed-probe", "read_file", `{"path":"zz_review_probe_failed.txt"}`)),
		turn(callBlock("cleanup-failed-probe", "delete_file", `{"path":"zz_review_probe_failed.txt"}`)),
		turn(textBlock("plausible: failed probe, exit 7; observed evidence read and scratch deleted; no confirmation claimed.")),
	}}
	s.model = model
	var out, errs bytes.Buffer
	c := &conversation{session: s, traceDir: t.TempDir()}
	chat(context.Background(), c, newLineReader(strings.NewReader("/review max\n/exit\n"), &errs, nil), &out, &errs)
	if len(model.requests) != 6 || s.blocked != "" {
		t.Fatalf("requests %d blocked %s: %s", len(model.requests), s.blocked, &errs)
	}
	var failed ToolOutcome
	for _, result := range results(s) {
		if result.CallID == "failed-probe" {
			failed = result.Outcome
		}
	}
	if failed.OK || !strings.Contains(string(failed.Data), `"exit_code":7`) || !strings.Contains(string(failed.Data), "probe assertion failed") {
		t.Fatalf("probe failure not observed: %+v", failed)
	}
	reviewToolResult(t, s.history, "read-failed-probe")
	reviewToolResult(t, s.history, "cleanup-failed-probe")
	if _, err := os.Stat(filepath.Join(s.cfg.Workspace.Root(), scratch)); !os.IsNotExist(err) {
		t.Fatalf("failed probe scratch remains: %v", err)
	}
	if !strings.Contains(out.String(), "failed probe, exit 7") || !strings.Contains(out.String(), "no confirmation claimed") {
		t.Fatal("failed probe report overclaimed success")
	}
}

func TestChat_ReviewChangedPagedBaselineDisclosesUnknownDelta(t *testing.T) {
	s := agentFixture(t)
	for i := 0; i < 300; i++ {
		id := fmt.Sprintf("prior-%03d", i)
		s.agents.archive[id] = agentObservation{AgentThread: AgentThread{ID: id, ParentID: "root", Task: strings.Repeat("prior task ", 20), Cost: Usage{Known: true, InputTokens: 1}}, Depth: 1}
	}
	step, offset := 0, 0
	var before, after agentSubtree
	seen := map[string]bool{}
	changed := false
	s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
		step++
		if after.TotalAgents != 0 {
			data(t, reviewToolResult(t, req.History, "after"), &after)
			return turn(textBlock(fmt.Sprintf("Changed paged baseline: totals %d -> %d, input %d -> %d; duplicate/missing IDs observed; collected unique IDs %d. Full final actual total %d, input %d; review delta unknown; no stable baseline or stable ID inventory claimed.", before.TotalAgents, after.TotalAgents, before.TotalCost.InputTokens, after.TotalCost.InputTokens, len(seen), after.TotalAgents, after.TotalCost.InputTokens))), nil
		}
		if step == 1 {
			for _, rule := range []string{"Capture totals before and after EACH paged view", "Offset", "NOT a stable snapshot", "unknown review delta"} {
				if !strings.Contains(req.History[0].User.Text, rule) {
					t.Errorf("prompt lacks changed-view rule %s", rule)
				}
			}
			return turn(callBlock("before", "threads", `{"subtree":true}`)), nil
		}
		id := fmt.Sprintf("page-%d", step-1)
		var page agentSubtree
		if step == 2 {
			data(t, reviewToolResult(t, req.History, "before"), &page)
			before = page
			if page.NextOffset == nil {
				t.Fatal("baseline fixture did not page")
			}
			// Simulate a pre-existing parent's new descendant after page zero.
			// An earlier-sort ID shifts offsets; no nested agent is launched here.
			s.agents.mu.Lock()
			s.agents.archive["a-new-descendant"] = agentObservation{AgentThread: AgentThread{ID: "a-new-descendant", ParentID: "prior-000", Cost: Usage{Known: true, InputTokens: 9}}, Depth: 2}
			s.agents.mu.Unlock()
		} else {
			data(t, reviewToolResult(t, req.History, id), &page)
		}
		if page.TotalAgents != before.TotalAgents || page.TotalCost != before.TotalCost {
			changed = true
		}
		for _, thread := range page.Threads {
			if seen[thread.ID] {
				changed = true
			}
			seen[thread.ID] = true
		}
		if page.NextOffset != nil {
			offset = *page.NextOffset
			raw, _ := json.Marshal(map[string]any{"subtree": true, "offset": offset})
			return turn(callBlock(fmt.Sprintf("page-%d", step), "threads", string(raw))), nil
		}
		after = page
		return turn(callBlock("after", "threads", `{"subtree":true}`)), nil
	}}
	var out, errs bytes.Buffer
	c := &conversation{session: s, traceDir: t.TempDir()}
	chat(context.Background(), c, newLineReader(strings.NewReader("/review medium\n/exit\n"), &errs, nil), &out, &errs)
	if !changed || s.blocked != "" {
		t.Fatalf("changed %v blocked %s: %s", changed, s.blocked, &errs)
	}
	for _, want := range []string{"Changed paged baseline", "totals 300 -> 301", "input 300 -> 309", "review delta unknown", "no stable baseline"} {
		if !strings.Contains(out.String(), want) {
			t.Errorf("instability report lacks %q: %s", want, &out)
		}
	}
}
