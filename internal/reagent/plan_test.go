package reagent

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestPlanMarkerFor(t *testing.T) {
	on := []Entry{{Kind: EntryUser, User: &UserTurn{Text: "plan", Plan: "on"}}, {Kind: EntryAssistant, Assistant: &ModelResponse{}}, {Kind: EntryShell, Shell: &ShellCommand{}}}
	if got := planMarkerFor(nil, true); got != "on" {
		t.Fatalf("on with no history: %q", got)
	}
	if got := planMarkerFor(on, false); got != "ended" {
		t.Fatalf("first message after plan mode: %q", got)
	}
	ended := append(append([]Entry(nil), on...), Entry{Kind: EntryUser, User: &UserTurn{Text: "implement", Plan: "ended"}})
	if got := planMarkerFor(ended, false); got != "" {
		t.Fatalf("second message after plan mode: %q", got)
	}
	if got := planMarkerFor(nil, false); got != "" {
		t.Fatalf("toggle with no turn: %q", got)
	}
}

func TestSession_PlanMarkersSurviveHistoryAndEndOnce(t *testing.T) {
	model := &requestRecorder{ScriptedModel: NewScriptedModel(turn(textBlock("plan")), turn(textBlock("do it")), turn(textBlock("done")))}
	cfg := testConfig(t)
	cfg.PlanMode = true
	session := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
	for i, text := range []string{"plan task", "implement", "continue"} {
		if i == 1 {
			session.planMode = false
		}
		result, err := session.Turn(context.Background(), text, string(rune('a'+i)), filepath.Join(t.TempDir(), "events.jsonl"))
		if err != nil || result.Status != StatusCompleted {
			t.Fatalf("turn %d: %+v, %v", i, result, err)
		}
	}
	for i, want := range []string{"on", "ended", ""} {
		user := model.requests[i].History[len(model.requests[i].History)-1].User
		if user.Plan != want || session.history[i*2].User.Plan != want {
			t.Fatalf("turn %d: request %+v, history %+v", i, user, session.history[i*2].User)
		}
	}
	if model.requests[2].History[0].User.Plan != "on" || model.requests[2].History[2].User.Plan != "ended" {
		t.Fatalf("old markers changed: %+v", model.requests[2].History)
	}
}

func TestDispatch_PlanModeRefusesWriteAndExec(t *testing.T) {
	root := t.TempDir()
	file := filepath.Join(root, "a.txt")
	marker := filepath.Join(root, "ran.txt")
	if err := os.WriteFile(file, []byte("before\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	ws, err := OpenWorkspace(root)
	if err != nil {
		t.Fatal(err)
	}
	cfg := testConfig(t, NewEditFileTool(ws), NewWriteFileTool(ws), NewDeleteFileTool(ws), NewExecTool(ws), NewReadFileTool(ws))
	cfg.PlanMode = true
	path := filepath.Join(t.TempDir(), "events.jsonl")
	session, result := oneTurn(t, context.Background(), cfg, NewScriptedModel(
		turn(
			callBlock("edit", "edit_file", `{}`),
			callBlock("write", "write_file", `{}`),
			callBlock("delete", "delete_file", `{}`),
			callBlock("exec", "exec", `{"argv":["touch","`+marker+`"],"cwd":"."}`),
			callBlock("read", "read_file", `{"path":"a.txt"}`),
		), turn(textBlock("plan"))), NewTrace(io.Discard), path, "plan it")
	if result.Status != StatusCompleted || result.ToolCalls != 5 || len(result.Effects) != 0 {
		t.Fatalf("result: %+v", result)
	}
	for i, name := range []string{"edit_file", "write_file", "delete_file", "exec"} {
		got := results(session)[i].Outcome
		want := name + " is refused in plan mode, which only the user can end. Put the change in the plan; to see a command's output, ask the user to run it with !."
		if got.OK || got.Code != "plan_mode" || got.Message != want || got.Effect != EffectNone {
			t.Fatalf("%s: %+v", name, got)
		}
	}
	if !results(session)[4].Outcome.OK {
		t.Fatalf("read was refused: %+v", results(session)[4])
	}
	started, finished := 0, 0
	for _, event := range readEvents(t, path) {
		switch event.Type {
		case "tool.started":
			started++
			data, _ := json.Marshal(event.Data)
			if !strings.Contains(string(data), `"name":"read_file"`) {
				t.Fatalf("refused tool started: %s", data)
			}
		case "tool.finished":
			finished++
		}
	}
	if started != 1 || finished != 5 {
		t.Fatalf("traced %d starts and %d finishes", started, finished)
	}
	if got, err := os.ReadFile(file); err != nil || string(got) != "before\n" {
		t.Fatalf("file: %q, %v", got, err)
	}
	if _, err := os.Stat(marker); !os.IsNotExist(err) {
		t.Fatalf("exec created marker: %v", err)
	}
}

func TestDispatch_PlanModeInReadOnlyStillSaysPermissionDenied(t *testing.T) {
	ws, err := OpenWorkspace(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	registry, err := NewRegistry(Mode{ReadOnly: true}, NewWriteFileTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	cfg := testConfig(t)
	cfg.Registry, cfg.PlanMode = registry, true
	session, result := runScript(t, cfg,
		turn(callBlock("write", "write_file", `{}`)), turn(textBlock("done")))
	if result.Status != StatusCompleted || results(session)[0].Outcome.Code != "permission_denied" {
		t.Fatalf("got %+v, result %+v", results(session), result)
	}
}

func TestEncode_PlanMarkerIsTheLastTextPart(t *testing.T) {
	for _, test := range []struct {
		provider string
		encode   func(ModelRequest) ([]byte, error)
	}{
		{openaiName, EncodeOpenAIRequest},
		{anthropicName, EncodeAnthropicRequest},
	} {
		t.Run(test.provider, func(t *testing.T) {
			user := &UserTurn{Text: "task", Workspace: json.RawMessage(`{"kind":"workspace_state"}`), Plan: "on"}
			req := ModelRequest{Model: "test", History: []Entry{{Kind: EntryUser, User: user}}}
			body, err := test.encode(req)
			if err != nil {
				t.Fatal(err)
			}
			var decoded struct {
				Input []struct {
					Content []struct {
						Text string `json:"text"`
					} `json:"content"`
				} `json:"input"`
				Messages []struct {
					Content []struct {
						Text string `json:"text"`
					} `json:"content"`
				} `json:"messages"`
			}
			if err := json.Unmarshal(body, &decoded); err != nil {
				t.Fatal(err)
			}
			var parts []struct {
				Text string `json:"text"`
			}
			if test.provider == openaiName {
				if len(decoded.Input) != 1 {
					t.Fatalf("input: %s", body)
				}
				parts = decoded.Input[0].Content
			} else {
				if len(decoded.Messages) != 1 {
					t.Fatalf("messages: %s", body)
				}
				parts = decoded.Messages[0].Content
			}
			if len(parts) != 3 || parts[0].Text != "task" || parts[1].Text != workspacePreamble+string(user.Workspace) || parts[2].Text != planMarker {
				t.Fatalf("parts: %+v", parts)
			}
			user.Plan = ""
			without, err := test.encode(req)
			if err != nil {
				t.Fatal(err)
			}
			fresh, err := test.encode(ModelRequest{Model: "test", History: []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task", Workspace: user.Workspace}}}})
			if err != nil || string(without) != string(fresh) {
				t.Fatalf("unused mode changed request: %v\n%s\n%s", err, without, fresh)
			}
		})
	}
}

func TestFindPlan(t *testing.T) {
	for _, test := range []struct {
		name, reply, want string
		found             bool
	}{
		{"one", "before\n<plan>\none\n</plan>\nafter", "one", true},
		{"last", "<plan>\none\n</plan>\n<plan>\ntwo\n</plan>", "two", true},
		{"unclosed", "<plan>\nunfinished", "", false},
		{"spaces", "<plan> \nnot a plan\n</plan>", "", false},
		{"closing spaces", "<plan>\nnot a plan\n</plan> ", "", false},
		{"last unclosed", "<plan>\none\n</plan>\n<plan>\ntwo", "", false},
		{"fenced", "```\n<plan>\ninside\n</plan>\n```", "inside", true},
	} {
		t.Run(test.name, func(t *testing.T) {
			start, end, got := findPlan(test.reply)
			if got != test.found || (got && planContent(test.reply[start:end]) != test.want) {
				t.Fatalf("got %q, found %t", planContent(test.reply[start:end]), got)
			}
		})
	}
}
