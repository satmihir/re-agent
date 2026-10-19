package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"
)

func TestAgents_ReportFrictionInherited(t *testing.T) {
	for _, fork := range []bool{false, true} {
		for _, enabled := range []bool{false, true} {
			t.Run(fmt.Sprintf("fork=%t/enabled=%t", fork, enabled), func(t *testing.T) {
				s := agentFixture(t)
				if enabled {
					reporter := NewReportFrictionTool()
					s.cfg.Registry.byName["report_friction"] = reporter
					s.cfg.Registry.specs = append(s.cfg.Registry.specs, reporter.Spec())
					s.cfg.ReportFriction = true
				}
				s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
					if cfg.ReportFriction != enabled {
						t.Errorf("child friction flag = %t", cfg.ReportFriction)
					}
					return NewScriptedModel(turn(callBlock("report", "report_friction", `{"category":"other","summary":"child rough edge"}`)), turn(textBlock("done"))), nil
				}
				s.model = NewScriptedModel(turn(callBlock("spawn", "spawn", fmt.Sprintf(`{"task":"report","name":"reporter","context":{"fork":%t}}`, fork))), turn(callBlock("wait", "wait", `{"ids":["reporter"]}`)), turn(textBlock("done")))
				agentTurn(t, s, "report friction")
				n := s.agents.nodes["t1"]
				var found bool
				for _, entry := range n.session.history {
					if entry.Tool == nil || entry.Tool.Name != "report_friction" {
						continue
					}
					found = true
					out := entry.Tool.Outcome
					if enabled && !out.OK || !enabled && (out.Code != "permission_denied" || out.Message != "report_friction is not available to agents") {
						t.Errorf("report outcome: %+v", out)
					}
				}
				if !found {
					t.Fatal("no report result")
				}
				data, err := os.ReadFile(n.TracePath)
				if err != nil {
					t.Fatal(err)
				}
				var attributed bool
				for _, line := range strings.Split(strings.TrimSpace(string(data)), "\n") {
					var e event
					if err := json.Unmarshal([]byte(line), &e); err != nil {
						t.Fatal(err)
					}
					if e.Type == "tool.finished" && strings.Contains(line, `"name":"report_friction"`) && e.AgentID == "t1" && e.ParentID == "root" {
						attributed = true
					}
				}
				if !attributed {
					t.Fatal("report not attributed to child")
				}
			})
		}
	}
}

func TestAgents_ReportFrictionSharesRootCap(t *testing.T) {
	s := agentFixture(t)
	reporter := NewReportFrictionTool()
	s.cfg.Registry.byName["report_friction"] = reporter
	s.cfg.Registry.specs = append(s.cfg.Registry.specs, reporter.Spec())
	s.cfg.ReportFriction = true
	s.agentModel = func(Config, *Trace) (Model, error) {
		return NewScriptedModel(turn(callBlock("child-report", "report_friction", `{"category":"other","summary":"child rough edge"}`)), turn(textBlock("done"))), nil
	}
	var reports []OutputBlock
	for i := 0; i < 9; i++ {
		reports = append(reports, callBlock(fmt.Sprint(i), "report_friction", `{"category":"other","summary":"root rough edge"}`))
	}
	s.model = NewScriptedModel(turn(reports...), turn(spawnBlock("spawn", "report", "reporter")), turn(callBlock("wait", "wait", `{"ids":["reporter"]}`)), turn(callBlock("last-report", "report_friction", `{"category":"other","summary":"eleventh"}`)), turn(textBlock("done")))
	agentTurn(t, s, "shared cap")
	for _, entry := range s.agents.nodes["t1"].session.history {
		if entry.Tool != nil && entry.Tool.Name == "report_friction" && !entry.Tool.Outcome.OK {
			t.Errorf("tenth report refused: %+v", entry.Tool.Outcome)
		}
	}
	for _, entry := range s.history {
		if entry.Tool != nil && entry.Tool.CallID == "last-report" && (entry.Tool.Outcome.OK || !strings.Contains(entry.Tool.Outcome.Message, "report limit reached")) {
			t.Errorf("eleventh report not capped: %+v", entry.Tool.Outcome)
		}
	}
}

func TestAgents_CapSummary(t *testing.T) {
	for _, cap := range []string{"steps", "calls", "calls-exact"} {
		for _, failed := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/failed=%t", cap, failed), func(t *testing.T) {
				s := agentFixture(t)
				requests, summaries := 0, 0
				s.agentModel = func(Config, *Trace) (Model, error) {
					return agentTestModel{generate: func(ctx context.Context, req ModelRequest) (ModelResponse, error) {
						requests++
						if req.MaxOutputTokens != 0 {
							summaries++
							encoded, _ := json.Marshal(req)
							var fields map[string]any
							if err := json.Unmarshal(encoded, &fields); err != nil {
								t.Fatal(err)
							}
							if fields["max_output_tokens"] != float64(8192) {
								t.Errorf("summary output budget = %v", fields["max_output_tokens"])
							}
							if deadline, ok := ctx.Deadline(); !ok || time.Until(deadline) > 2*time.Minute {
								t.Error("summary request lacks bounded deadline")
							}
							if policy, ok := ctx.Value(modelRetryKey{}).(modelRetry); !ok || policy.window != 0 {
								t.Error("summary retry window is not bounded to one attempt")
							}
							prompt := req.History[len(req.History)-1].User.Text
							for _, want := range []string{"what was done", "effects", "tests", "remains"} {
								if !strings.Contains(prompt, want) {
									t.Errorf("summary prompt missing %q: %s", want, prompt)
								}
							}
							if failed {
								return ModelResponse{}, &ModelError{Status: StatusProviderError, Message: "summary failed", Usage: Usage{Known: true, InputTokens: 7, CachedInputTokens: 2, OutputTokens: 3, ReasoningTokens: 1}}
							}
							resp := turn(textBlock("Wrote result.txt; tests not run; review remains."))
							resp.Usage = Usage{Known: true, InputTokens: 7, CachedInputTokens: 2, OutputTokens: 3, ReasoningTokens: 1}
							return resp, nil
						}
						var blocks []OutputBlock
						blocks = append(blocks, textBlock("last progress"))
						if req.Scope.Step == 1 {
							blocks = append(blocks, callBlock("write", "write_file", `{"path":"result.txt","content":"applied"}`))
						}
						n := 1
						if strings.HasPrefix(cap, "calls") {
							n = maxAgentCalls
							if cap == "calls-exact" {
								n--
							}
						}
						for i := 0; i < n; i++ {
							blocks = append(blocks, callBlock(fmt.Sprintf("list-%d-%d", req.Scope.Step, i), "list_files", `{"path":"."}`))
						}
						// Leave room to apply the write before overflowing the next batch.
						if cap == "calls" && req.Scope.Step == 1 {
							blocks = blocks[:3]
						}
						resp := turn(blocks...)
						resp.Usage = Usage{Known: true, InputTokens: 1}
						return resp, nil
					}}, nil
				}
				s.model = NewScriptedModel(turn(callBlock("spawn", "spawn", `{"task":"work","name":"worker","authority":"write","context":{}}`)), turn(callBlock("wait", "wait", `{"ids":["worker"]}`)), turn(textBlock("done")))
				agentTurn(t, s, "caps")
				results := agentResults(s.history)
				if len(results) != 1 {
					t.Fatalf("results: %+v", results)
				}
				result := results[0].Result
				want := "Wrote result.txt; tests not run; review remains."
				if failed {
					want = "last progress"
				}
				wantRequests := maxAgentSteps + 1
				if strings.HasPrefix(cap, "calls") {
					wantRequests = 3
				}
				if requests != wantRequests || summaries != 1 || result.Steps != requests || result.Status != StatusLimitExceeded || result.Reply != want {
					t.Errorf("requests=%d summaries=%d result=%+v", requests, summaries, result)
				}
				if files := results[0].Effects.Files; len(files) != 1 || files[0].Path != "result.txt" {
					t.Errorf("applied effects lost: %+v", results[0].Effects)
				}
				if result.Usage.InputTokens != int64(requests-1+7) || result.Usage.CachedInputTokens != 2 || result.Usage.OutputTokens != 3 || result.Usage.ReasoningTokens != 1 {
					t.Errorf("summary usage lost: %+v requests=%d", result.Usage, requests)
				}
				data, err := os.ReadFile(results[0].Result.TracePath)
				if err != nil {
					t.Fatal(err)
				}
				terminal := "accepted"
				if failed {
					terminal = "failed"
				}
				if !strings.Contains(string(data), `"type":"agent.cap_summary.requested"`) || !strings.Contains(string(data), `"type":"agent.cap_summary.`+terminal+`"`) {
					t.Error("cap summary trace marker missing")
				}
			})
		}
	}
}

func TestAgents_ExactlyCallCapCanComplete(t *testing.T) {
	s := agentFixture(t)
	var calls []OutputBlock
	for i := 0; i < maxAgentCalls; i++ {
		calls = append(calls, callBlock(fmt.Sprint(i), "list_files", `{"path":"."}`))
	}
	s.agentModel = func(Config, *Trace) (Model, error) {
		return NewScriptedModel(turn(calls...), turn(textBlock("final answer"))), nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("spawn", "work", "worker")), turn(callBlock("wait", "wait", `{"ids":["worker"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "exact call cap")
	result := agentResults(s.history)[0].Result
	if result.Status != StatusCompleted || result.Reply != "final answer" || result.ToolCalls != maxAgentCalls || result.Steps != 2 {
		t.Fatalf("normal completion at call cap: %+v", result)
	}
	data, err := os.ReadFile(result.TracePath)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(data), "agent.cap_summary") {
		t.Fatal("normal completion requested a cap summary")
	}
}

func TestAgents_CapSummaryKeepsEncodedDeclarations(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		for _, fork := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/fork=%t", provider, fork), func(t *testing.T) {
				s := agentFixture(t)
				s.cfg.Provider = provider
				if provider == anthropicName {
					s.cfg.Model = DefaultAnthropicModel
				}
				encode := EncodeOpenAIRequest
				if provider == anthropicName {
					encode = EncodeAnthropicRequest
				}
				var previous map[string]json.RawMessage
				summaries := 0
				s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
					if fork && cfg.agentPrefix == nil {
						t.Error("native fork prefix missing")
					}
					if _, found := cfg.Registry.Lookup("write_file"); found {
						t.Error("read agent unexpectedly has writer authority")
					}
					return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
						body, err := encode(req)
						if err != nil {
							t.Errorf("encode cap history: %v", err)
							return ModelResponse{}, err
						}
						var fields map[string]json.RawMessage
						if err := json.Unmarshal(body, &fields); err != nil {
							t.Fatal(err)
						}
						if req.MaxOutputTokens != 0 {
							summaries++
							if len(fields["tools"]) == 0 || string(fields["tools"]) != string(previous["tools"]) {
								t.Errorf("cap summary changed encoded declarations: got %d bytes; want %d", len(fields["tools"]), len(previous["tools"]))
							}
							if strings.Contains(string(fields["tools"]), `"name":"write_file"`) != fork {
								t.Error("fork writer declarations not retained")
							}
							for _, kind := range []string{"function_call", "function_call_output"} {
								if provider == anthropicName {
									if kind == "function_call" {
										kind = "tool_use"
									} else {
										kind = "tool_result"
									}
								}
								if !strings.Contains(string(body), `"type":"`+kind+`"`) {
									t.Errorf("encoded summary missing %s history", kind)
								}
							}
							return contextResponse(provider, textBlock("cap summary")), nil
						}
						previous = fields
						return contextResponse(provider, callBlock(fmt.Sprint(req.Scope.Step), "list_files", `{"path":"."}`)), nil
					}}, nil
				}
				s.model = NewScriptedModel(contextResponse(provider, callBlock("spawn", "spawn", fmt.Sprintf(`{"task":"work","name":"worker","context":{"fork":%t}}`, fork))), contextResponse(provider, callBlock("wait", "wait", `{"ids":["worker"]}`)), contextResponse(provider, textBlock("done")))
				agentTurn(t, s, "summary declarations")
				result := agentResults(s.history)[0].Result
				if summaries != 1 || result.Status != StatusLimitExceeded || result.Reply != "cap summary" {
					t.Fatalf("summary delivery: summaries=%d result=%+v", summaries, result)
				}
			})
		}
	}
}

func TestAgents_CapSummaryDoesNotDispatchCalls(t *testing.T) {
	s := agentFixture(t)
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			if req.MaxOutputTokens != 0 {
				return turn(callBlock("summary-write", "write_file", `{"path":"unexpected.txt","content":"must not run"}`)), nil
			}
			return turn(textBlock("last progress"), callBlock(fmt.Sprint(req.Scope.Step), "list_files", `{"path":"."}`)), nil
		}}, nil
	}
	s.model = NewScriptedModel(turn(callBlock("spawn", "spawn", `{"task":"work","name":"worker","authority":"write","context":{}}`)), turn(callBlock("wait", "wait", `{"ids":["worker"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "reject summary tools")
	result := agentResults(s.history)[0]
	if result.Result.Status != StatusLimitExceeded || result.Result.Reply != "last progress" || len(result.Effects.Files) != 0 {
		t.Fatalf("summary tool call not rejected: %+v", result)
	}
	if _, err := os.Stat(s.cfg.WorkspacePath + "/unexpected.txt"); !os.IsNotExist(err) {
		t.Fatalf("summary write ran: %v", err)
	}
	data, err := os.ReadFile(result.Result.TracePath)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), "cap summary must not call tools") || strings.Contains(string(data), `"call_id":"summary-write","name":"write_file","outcome"`) {
		t.Fatal("summary tool rejection missing or dispatched")
	}
}
