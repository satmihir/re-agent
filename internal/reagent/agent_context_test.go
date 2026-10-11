package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func contextResponse(provider string, blocks ...OutputBlock) ModelResponse {
	response := turn(blocks...)
	response.Native.Provider = openaiProvider
	if provider == anthropicName {
		response.Native.Provider = anthropicProvider
	}
	for _, block := range blocks {
		var item any
		if block.Call != nil {
			if provider == openaiName {
				item = map[string]any{"type": "function_call", "call_id": block.Call.CallID, "name": block.Call.Name, "arguments": block.Call.Arguments}
			} else {
				item = map[string]any{"type": "tool_use", "id": block.Call.CallID, "name": block.Call.Name, "input": json.RawMessage(block.Call.Arguments)}
			}
		} else if provider == openaiName {
			item = map[string]any{"type": "message", "role": "assistant", "content": []any{map[string]any{"type": "output_text", "text": block.Text}}}
		} else {
			item = map[string]any{"type": "text", "text": block.Text}
		}
		raw, _ := json.Marshal(item)
		response.Native.Items = append(response.Native.Items, raw)
	}
	return response
}

func forkBlock(id, model, authority string, context *agentContext) OutputBlock {
	raw, _ := json.Marshal(spawnArgs{Task: "independent task", Name: "fork", Model: model, Authority: authority, Context: context})
	return callBlock(id, "spawn", string(raw))
}

func TestAgents_ForkEncodedPrefixAndUnansweredBoundary(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		for _, toolBoundary := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/tools=%t", provider, toolBoundary), func(t *testing.T) {
				s := agentFixture(t)
				s.cfg.Provider = provider
				if provider == anthropicName {
					s.cfg.Model = DefaultAnthropicModel
				}
				var parent ModelRequest
				var child ModelRequest
				s.agentModel = func(Config, *Trace) (Model, error) {
					return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
						child = req
						return contextResponse(provider, textBlock("fork done")), nil
					}}, nil
				}
				s.model = agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					if toolBoundary && req.Scope.Step == 1 {
						return contextResponse(provider, callBlock("previous", "list_files", `{"path":"."}`)), nil
					}
					if len(agentResults(req.History)) > 0 {
						return contextResponse(provider, textBlock("parent done")), nil
					}
					for _, entry := range req.History {
						if entry.Tool != nil && entry.Tool.Name == "spawn" {
							return contextResponse(provider, callBlock("waiting", "wait", `{"ids":["fork"]}`)), nil
						}
					}
					parent = req
					// The earlier result in this same batch must also be excluded.
					return contextResponse(provider, callBlock("same-batch", "list_files", `{"path":"."}`), forkBlock("spawning", s.cfg.Model, "read", &agentContext{Fork: true})), nil
				}}
				agentTurn(t, s, "fork this")
				if len(child.History) != len(parent.History)+1 {
					t.Fatalf("boundary lengths: parent %d child %d", len(parent.History), len(child.History))
				}
				if child.Instructions != parent.Instructions || openAICacheKey(child) != openAICacheKey(parent) {
					t.Fatal("fixed instructions or cache key changed")
				}
				parentBytes, err := encodeRequest(s.cfg, parent)
				if err != nil {
					t.Fatal(err)
				}
				childBytes, err := encodeRequest(s.cfg, child)
				if err != nil {
					t.Fatal(err)
				}
				var p, c map[string]json.RawMessage
				json.Unmarshal(parentBytes, &p)
				json.Unmarshal(childBytes, &c)
				if !bytes.Equal(p["tools"], c["tools"]) || !bytes.Equal(p["prompt_cache_key"], c["prompt_cache_key"]) {
					t.Fatal("declaration bytes or encoded cache key differ")
				}
				field := "input"
				trim := 1 // The enclosing array is extended, not part of its items.
				if provider == anthropicName {
					field, trim = "messages", 3 // Adjacent user content extends the last message.
				}
				end := bytes.Index(parentBytes, p[field]) + len(p[field]) - trim
				if !bytes.HasPrefix(childBytes, parentBytes[:end]) {
					t.Fatalf("encoded request prefix differs through parent's last history block\nparent: %s\nchild: %s", parentBytes, childBytes)
				}
				if strings.Contains(string(childBytes), "spawning") || strings.Contains(string(childBytes), "same-batch") {
					t.Fatal("unanswered current batch reached fork")
				}
				if !strings.Contains(child.History[len(child.History)-1].User.Text, agentInstructions) {
					t.Fatal("agent-specific instructions not in appended task")
				}
			})
		}
	}
}

func timeoutHistory(code string) []Entry {
	return []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "run a command"}},
		{Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{callBlock("timed", "exec", `{"argv":["true"],"cwd":"."}`)}}},
		{Kind: EntryTool, Tool: &ToolResult{CallID: "timed", Name: "exec", Outcome: ToolOutcome{Code: code, Effect: EffectUnknown}}},
	}
}

func TestAgents_ContextUncertainEffectsAndSelectedData(t *testing.T) {
	for _, mode := range []string{"fork", "selected"} {
		for _, state := range []string{"unseen_timeout", "acknowledged_timeout", "other_unknown"} {
			t.Run(mode+"/"+state, func(t *testing.T) {
				s := agentFixture(t)
				code := "timeout"
				if state == "other_unknown" {
					code = "cancelled"
				}
				s.history = timeoutHistory(code)
				if state != "unseen_timeout" {
					s.history = append(s.history, Entry{Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{textBlock("seen")}}})
				}
				sharing := &agentContext{Fork: mode == "fork"}
				if mode == "selected" {
					sharing.Entries = []int{3, 1}
				}
				// Projection avoids requiring synthetic source-native items here.
				n, err := s.spawnAgent(context.Background(), spawnArgs{Task: "inspect", Model: DefaultAnthropicModel, Context: sharing})
				if state != "acknowledged_timeout" {
					if err == nil || !strings.Contains(err.Error(), "uncertain") {
						t.Fatalf("wanted refusal: %+v %v", n, err)
					}
					return
				}
				if err != nil {
					t.Fatal(err)
				}
				if _, err := s.waitAgents(context.Background(), []string{n.ID}); err != nil {
					t.Fatal(err)
				}
				child := s.agents.nodes[n.ID].session
				if mode == "selected" {
					text := child.history[0].User.Text
					if !strings.Contains(text, `"entry":3`) || !strings.Contains(text, `"trust":"untrusted_tool_data"`) || strings.Contains(text, `"entry":2`) || strings.Index(text, `"entry":3`) > strings.Index(text, `"entry":1`) {
						t.Fatalf("bad selected data: %s", text)
					}
				}
			})
		}
	}
}

func TestAgents_ForkFallbackAdmissionAndFixedHistory(t *testing.T) {
	for _, target := range []string{DefaultAnthropicModel, DefaultOpenAIModel} {
		t.Run(target, func(t *testing.T) {
			s := agentFixture(t)
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "original"}}, {Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{textBlock("visible")}, Native: NativeOutput{Provider: openaiName, Items: []json.RawMessage{json.RawMessage(`{"type":"reasoning","encrypted_content":"secret"}`)}}}}}
			n, err := s.spawnAgent(context.Background(), spawnArgs{Task: "inspect", Model: target, Context: &agentContext{Fork: true}})
			if err != nil {
				t.Fatal(err)
			}
			s.history[0].User.Text = "parent changed"
			s.history = append(s.history, Entry{Kind: EntryUser, User: &UserTurn{Text: "later"}})
			if _, err := s.waitAgents(context.Background(), []string{n.ID}); err != nil {
				t.Fatal(err)
			}
			child := s.agents.nodes[n.ID].session
			if n.ContextMode != "fork_text" || len(child.history) != 3 || strings.Contains(child.history[0].User.Text, "secret") || strings.Contains(child.history[0].User.Text, "parent changed") || !strings.Contains(child.history[0].User.Text, `"omitted_native_items":1`) {
				t.Fatalf("bad fallback: %+v %+v", n, child.history)
			}
		})
	}
	s := agentFixture(t)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: strings.Repeat("x", 600_000)}}}
	_, err := s.spawnAgent(context.Background(), spawnArgs{Task: "inspect", Model: DefaultAnthropicModel, Context: &agentContext{Fork: true}})
	if err == nil || !strings.Contains(err.Error(), "window") {
		t.Fatalf("admission: %v", err)
	}
	if len(s.agentRoster()) != 0 {
		t.Fatal("failed admission published child")
	}
}

func TestAgents_ForkAuthorityAndReset(t *testing.T) {
	for _, authority := range []string{"read", "write"} {
		t.Run(authority, func(t *testing.T) {
			s := agentFixture(t)
			s.agentModel = func(Config, *Trace) (Model, error) {
				return NewScriptedModel(turn(callBlock("write", "write_file", `{"path":"child.txt","content":"ok"}`)), turn(callBlock("workspace", "switch_workspace", `{"path":"/tmp"}`)), turn(textBlock("done"))), nil
			}
			s.model = NewScriptedModel(turn(forkBlock("spawn", "", authority, &agentContext{Fork: true})), turn(callBlock("wait", "wait", `{"ids":["fork"]}`)), turn(textBlock("done")))
			agentTurn(t, s, "fork")
			child := s.agents.nodes["t1"].session
			for _, e := range child.history {
				if e.Tool == nil {
					continue
				}
				if e.Tool.Name == "write_file" && e.Tool.Outcome.OK != (authority == "write") {
					t.Fatalf("authority: %+v", e.Tool)
				}
				if e.Tool.Name == "switch_workspace" && e.Tool.Outcome.OK {
					t.Fatal("child widened workspace authority")
				}
			}
			_, err := os.Stat(filepath.Join(s.cfg.WorkspacePath, "child.txt"))
			if (err == nil) != (authority == "write") {
				t.Fatalf("file effect: %v", err)
			}
			child.Reset()
			if child.cfg.agentPrefix != nil || !strings.Contains(instructions(child.cfg), agentInstructions) {
				t.Fatal("reset lost agent rules or retained history markers")
			}
		})
	}
}

func TestAgents_ContextArgumentIndices(t *testing.T) {
	for _, context := range []string{`{"fork":true,"entries":[1]}`, `{"entries":[0]}`, `{"entries":[2]}`, `{"entries":[1,1]}`, `{"fork":"yes"}`} {
		t.Run(context, func(t *testing.T) {
			s := agentFixture(t)
			s.model = NewScriptedModel(turn(callBlock("spawn", "spawn", `{"task":"inspect","context":`+context+`}`)), turn(textBlock("done")))
			agentTurn(t, s, "indices")
			for _, entry := range s.history {
				if entry.Tool != nil && entry.Tool.Name == "spawn" && entry.Tool.Outcome.OK {
					t.Fatal("invalid context admitted")
				}
			}
		})
	}
}

func TestAgents_CurrentSpawnResponseAcknowledgesTimeout(t *testing.T) {
	s := agentFixture(t)
	s.history = timeoutHistory("timeout")
	s.history = append(s.history, Entry{Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{forkBlock("spawn", DefaultAnthropicModel, "read", &agentContext{Fork: true})}}})
	s.currentRun = newRun(s, "current")
	n, err := s.spawnAgent(context.Background(), spawnArgs{Task: "inspect timeout", Model: DefaultAnthropicModel, Context: &agentContext{Fork: true}})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.waitAgents(context.Background(), []string{n.ID}); err != nil {
		t.Fatal(err)
	}
	text := s.agents.nodes[n.ID].session.history[0].User.Text
	if !strings.Contains(text, `"code":"timeout"`) || strings.Contains(text, `"entry":4`) || strings.Contains(text, "validation only") {
		t.Fatalf("bad complete boundary: %s", text)
	}
}

func TestAgents_FreshContextStillHasStageOneAndTwoContracts(t *testing.T) {
	for _, authority := range []string{"read", "write"} {
		t.Run(authority, func(t *testing.T) {
			s := agentFixture(t)
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "parent secret"}}}
			// A non-fork child of a fork must not inherit its declaration override.
			s.cfg.agentPrefix = &agentRequestPrefix{Instructions: "inherited prefix"}
			s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
				if cfg.agentPrefix != nil {
					t.Error("fresh child inherited fork prefix")
				}
				return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					if len(req.History) != 1 || strings.Contains(req.History[0].User.Text, "parent secret") || !strings.Contains(req.Instructions, defaultInstructions) || !strings.Contains(req.Instructions, agentInstructions) {
						t.Error("fresh context contract changed")
					}
					writer := false
					for _, spec := range req.Tools {
						if spec.Name == "write_file" {
							writer = true
						}
					}
					if writer != (authority == "write") {
						t.Error("fresh authority declarations changed")
					}
					return turn(textBlock("done")), nil
				}}, nil
			}
			n, err := s.spawnAgent(context.Background(), spawnArgs{Task: "fresh", Authority: authority, Context: &agentContext{Brief: "brief", Files: []string{"AGENTS.md"}}})
			if err != nil {
				t.Fatal(err)
			}
			if _, err := s.waitAgents(context.Background(), []string{n.ID}); err != nil {
				t.Fatal(err)
			}
			if n.ContextMode != "fresh" {
				t.Fatal(n.ContextMode)
			}
		})
	}
}

func TestAgents_OffRefusesHistorySharingWithoutEffects(t *testing.T) {
	s := agentFixture(t)
	s.cfg.Agents = false
	s.model = NewScriptedModel(turn(forkBlock("fork", "", "write", &agentContext{Fork: true})), turn(forkBlock("select", "", "read", &agentContext{Entries: []int{1}})), turn(textBlock("done")))
	agentTurn(t, s, "disabled")
	for _, entry := range s.history {
		if entry.Tool != nil && (entry.Tool.Outcome.Code != "permission_denied" || entry.Tool.Outcome.Effect != EffectNone) {
			t.Fatalf("agents-off dispatch: %+v", entry.Tool)
		}
	}
	if len(s.agentRoster()) != 0 {
		t.Fatal("agents-off started children")
	}
}

func TestAgents_ForkNativeHistoryIsOwnedAndPlanInherited(t *testing.T) {
	s := agentFixture(t)
	s.planMode = true
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "original"}}, {Kind: EntryAssistant, Assistant: func() *ModelResponse { r := contextResponse(openaiName, textBlock("native")); return &r }()}}
	s.agentModel = func(Config, *Trace) (Model, error) {
		return NewScriptedModel(turn(callBlock("write", "write_file", `{"path":"plan.txt","content":"no"}`)), turn(textBlock("done"))), nil
	}
	n, err := s.spawnAgent(context.Background(), spawnArgs{Task: "planning", Authority: "write", Context: &agentContext{Fork: true}})
	if err != nil {
		t.Fatal(err)
	}
	s.history[0].User.Text = "mutated"
	s.history[1].Assistant.Native.Items[0][0] = ' '
	if _, err := s.waitAgents(context.Background(), []string{n.ID}); err != nil {
		t.Fatal(err)
	}
	child := s.agents.nodes[n.ID].session
	if child.history[0].User.Text != "original" || child.history[1].Assistant.Native.Items[0][0] != '{' {
		t.Fatal("fork history aliases parent")
	}
	if child.history[4].Tool.Outcome.Code != "plan_mode" {
		t.Fatalf("plan not inherited: %+v", child.history)
	}
}
