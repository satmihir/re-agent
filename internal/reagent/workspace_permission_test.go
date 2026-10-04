package reagent

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestWorkspaceConsent_ChatApprovesWithinTurnAndRetainsGrantAfterReset(t *testing.T) {
	root, target := t.TempDir(), t.TempDir()
	if err := os.WriteFile(filepath.Join(target, "a.txt"), []byte("target"), 0o600); err != nil {
		t.Fatal(err)
	}
	call := func(id, path string) ModelResponse {
		return turn(callBlock(id, "switch_workspace", string(mustJSON(t, map[string]string{"path": path}))))
	}
	model := &compactModel{replies: []ModelResponse{call("switch", target), turn(callBlock("read", "read_file", `{"path":"a.txt"}`)), turn(textBlock("first")), call("back", root), call("again", target), turn(textBlock("second"))}}
	s := workspaceSession(t, root, Mode{}, model)
	oldID := s.ID
	input := &fakeLineReader{chosen: 0}
	input.reads = []struct {
		line string
		err  error
	}{{line: "work"}, {line: "/reset"}, {line: "/status"}, {line: "again"}, {line: "/exit"}}
	var out, progress bytes.Buffer
	c := &conversation{session: s, scripted: model, trace: s.trace, traceDir: t.TempDir(), progress: &progress, usage: Usage{Known: true}}
	if code := chat(context.Background(), c, input, &out, &progress); code != exitOK {
		t.Fatal(code)
	}
	if input.chooseCalls != 1 || input.choiceCurrent != 1 || input.choiceConfig.shortcuts || !input.choiceConfig.freshInput || len(input.choices) != 2 || input.choices[0].label != "Allow for this session" || input.choices[1].label != "Deny" {
		t.Fatalf("picker %+v", input)
	}
	if s.ID == oldID || out.String() != "first\nsecond\n" || s.blocked != "" {
		t.Fatalf("chat %q, blocked %q", out.String(), s.blocked)
	}
	for _, text := range []string{s.cfg.WorkspacePath, "until this chat exits", "provider", "traces", "approved"} {
		if !strings.Contains(progress.String(), text) {
			t.Fatalf("missing %q in consent/status", text)
		}
	}
	c.commandShell(context.Background(), "pwd", input, &out, &progress)
	last := s.history[len(s.history)-1].Shell
	if last == nil || last.Workspace != s.cfg.WorkspacePath || strings.TrimSpace(last.Output) != s.cfg.WorkspacePath {
		t.Fatalf("shell %+v", last)
	}
}

func TestWorkspaceConsent_DenialCancellationAndUnavailableInputDoNotGrant(t *testing.T) {
	for _, tc := range []struct {
		name   string
		chosen int
		err    error
		code   string
	}{
		{"deny", 1, nil, "permission_denied"}, {"escape", 0, errCancelled, "permission_denied"},
		{"EOF", 0, io.EOF, "permission_denied"}, {"error", 0, errors.New("input failed"), "permission_denied"},
		{"noninteractive", 0, errNotInteractive, "permission_required"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root, target := t.TempDir(), t.TempDir()
			model := NewScriptedModel(turn(callBlock("switch", "switch_workspace", string(mustJSON(t, map[string]string{"path": target})))), turn(textBlock("kept working")))
			s := workspaceSession(t, root, Mode{}, model)
			before := s.cfg
			input := &fakeLineReader{chosen: tc.chosen, chooseErr: tc.err}
			input.reads = []struct {
				line string
				err  error
			}{{line: "work"}, {line: "/exit"}}
			var out, progress bytes.Buffer
			c := &conversation{session: s, scripted: model, traceDir: t.TempDir(), progress: &progress}
			if code := chat(context.Background(), c, input, &out, &progress); code != exitOK {
				t.Fatal(code)
			}
			if s.cfg.WorkspacePath != before.WorkspacePath || s.cfg.Registry != before.Registry || s.cfg.ProjectInstructions != before.ProjectInstructions || len(s.workspace.approved) != 1 || s.blocked != "" {
				t.Fatal("denial changed state or blocked chat")
			}
			if result := results(s)[0].Outcome; result.Code != tc.code || result.Workspace != before.WorkspacePath {
				t.Fatal(result)
			}
		})
	}
}

func TestWorkspaceConsent_CancellationOrDestinationChangeDuringPickerCannotCommit(t *testing.T) {
	for _, change := range []string{"cancel", "remove", "replace parent"} {
		t.Run(change, func(t *testing.T) {
			root, parent := t.TempDir(), t.TempDir()
			target := filepath.Join(parent, "target")
			if err := os.Mkdir(target, 0o700); err != nil {
				t.Fatal(err)
			}
			s := workspaceSession(t, root, Mode{}, NewScriptedModel())
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) {
				switch change {
				case "cancel":
					cancel()
				case "remove":
					if err := os.Remove(target); err != nil {
						t.Fatal(err)
					}
				case "replace parent":
					if err := os.Rename(parent, parent+"-old"); err != nil {
						t.Fatal(err)
					}
					t.Cleanup(func() { os.RemoveAll(parent + "-old") })
					if err := os.MkdirAll(target, 0o700); err != nil {
						t.Fatal(err)
					}
				}
				return true, nil
			}
			tool, _ := s.cfg.Registry.Lookup("switch_workspace")
			before := s.cfg
			outcome, err := tool.Execute(ctx, mustJSON(t, map[string]string{"path": target}))
			if err != nil || outcome.OK || s.cfg.Registry != before.Registry || s.cfg.WorkspacePath != before.WorkspacePath || len(s.workspace.approved) != 1 {
				t.Fatalf("committed: %+v %v", outcome, err)
			}
		})
	}
}

func TestWorkspaceConsent_NoninteractiveDoesNotConsumeNextMessage(t *testing.T) {
	root, target := t.TempDir(), t.TempDir()
	model := &compactModel{replies: []ModelResponse{turn(callBlock("switch", "switch_workspace", string(mustJSON(t, map[string]string{"path": target})))), turn(textBlock("denied")), turn(textBlock("ordinary message"))}}
	s := workspaceSession(t, root, Mode{}, model)
	var out, progress bytes.Buffer
	c := &conversation{session: s, scripted: model, traceDir: t.TempDir(), progress: &progress}
	if code := chat(context.Background(), c, newLineReader(strings.NewReader("work\nAllow for this session\n/exit\n"), &progress, nil), &out, &progress); code != exitOK {
		t.Fatal(code)
	}
	if len(model.requests) != 3 || model.requests[2].History[len(model.requests[2].History)-1].User.Text != "Allow for this session" || results(s)[0].Outcome.Code != "permission_required" {
		t.Fatal("prompt input was consumed as consent")
	}
}

func TestWorkspaceConsent_ReadOnlyAndPlanRemainEnforced(t *testing.T) {
	for _, tc := range []struct {
		name string
		mode Mode
		plan bool
		code string
	}{
		{"read-only", Mode{ReadOnly: true}, false, "permission_denied"}, {"plan", Mode{}, true, "plan_mode"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root, target := t.TempDir(), t.TempDir()
			model := NewScriptedModel(turn(callBlock("switch", "switch_workspace", string(mustJSON(t, map[string]string{"path": target})))), turn(callBlock("write", "write_file", `{"path":"oops","content":"bad"}`), callBlock("exec", "exec", `{"argv":["sh","-c","touch oops"],"cwd":"."}`)), turn(textBlock("planned")))
			s := workspaceSession(t, root, tc.mode, model)
			s.planMode = tc.plan
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
			result, err := s.Turn(context.Background(), "work", "run", filepath.Join(t.TempDir(), "events.jsonl"))
			if err != nil || result.Status != StatusCompleted {
				t.Fatalf("%+v %v", result, err)
			}
			for _, observation := range results(s)[1:] {
				if observation.Outcome.Code != tc.code || observation.Outcome.Workspace != s.cfg.WorkspacePath {
					t.Fatal(observation)
				}
			}
			for _, event := range readEvents(t, result.TracePath) {
				if event.Type == "tool.started" && event.Data.(map[string]any)["name"] != "switch_workspace" {
					t.Fatal("refused tool started")
				}
			}
			if _, err := os.Stat(filepath.Join(target, "oops")); !os.IsNotExist(err) {
				t.Fatal("restrictions lifted after consent")
			}
		})
	}
}

func TestWorkspaceConsent_ResetModelAndCompactionKeepCurrentRootAndCopy(t *testing.T) {
	root, target := t.TempDir(), t.TempDir()
	if err := os.WriteFile(filepath.Join(target, "AGENTS.md"), []byte("selected instructions"), 0o600); err != nil {
		t.Fatal(err)
	}
	s := workspaceSession(t, root, Mode{}, NewScriptedModel())
	prompts := 0
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { prompts++; return true, nil }
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	current := s.cfg
	if err := os.WriteFile(filepath.Join(target, "AGENTS.md"), []byte("later disk instructions"), 0o600); err != nil {
		t.Fatal(err)
	}
	s.planMode = true
	s.Reset()
	if s.cfg.WorkspacePath != current.WorkspacePath || s.cfg.ProjectInstructions != current.ProjectInstructions || !s.planMode {
		t.Fatal("reset discarded selection")
	}
	model := &compactModel{replies: []ModelResponse{turn(textBlock("handoff"))}}
	s.model = model
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "earlier"}}}
	result, _, err := s.Compact(context.Background(), "", "compact", filepath.Join(t.TempDir(), "events.jsonl"))
	if err != nil || result.Status != StatusCompleted || !strings.Contains(model.requests[0].Instructions, "selected instructions") {
		t.Fatalf("compaction %+v %v", result, err)
	}
	c := &conversation{session: s, trace: s.trace, client: NewHTTPClient(), progress: io.Discard}
	info, _ := findModel("claude-haiku-4-5")
	c.switchTo(info)
	if c.session.workspace == s.workspace || c.session.cfg.WorkspacePath != current.WorkspacePath || c.session.cfg.ProjectInstructions != current.ProjectInstructions || !c.session.planMode {
		t.Fatal("model change discarded selection or shared mutable state")
	}
	if outcome := workspaceTool(t, c.session, "switch_workspace", map[string]string{"path": root}); !outcome.OK {
		t.Fatal(outcome)
	}
	if outcome := workspaceTool(t, c.session, "switch_workspace", map[string]string{"path": target}); !outcome.OK || prompts != 1 {
		t.Fatalf("%+v prompts %d", outcome, prompts)
	}
	if s.cfg.WorkspacePath != current.WorkspacePath {
		t.Fatal("replacement moved the previous session")
	}
}

func TestWorkspaceConsent_LaunchPreapprovalWorksOffline(t *testing.T) {
	root, target := t.TempDir(), t.TempDir()
	script := filepath.Join(t.TempDir(), "script.json")
	responses := []ModelResponse{turn(callBlock("switch", "switch_workspace", string(mustJSON(t, map[string]string{"path": target})))), turn(callBlock("pwd", "exec", `{"argv":["pwd"],"cwd":"."}`)), turn(textBlock("done"))}
	if err := os.WriteFile(script, mustJSON(t, responses), 0o600); err != nil {
		t.Fatal(err)
	}
	trace := filepath.Join(t.TempDir(), "events.jsonl")
	var out, progress bytes.Buffer
	args := []string{"run", "--workspace", root, "--allow-workspace", target, "--allow-workspace", filepath.Join(target, "future"), "--scripted", script, "--trace-file", trace, "work there"}
	if code := Main(context.Background(), args, strings.NewReader(""), &out, &progress); code != exitOK {
		t.Fatalf("exit %d %s", code, progress.String())
	}
	if out.String() != "done\n" || strings.Contains(progress.String(), "Allow this location") {
		t.Fatalf("preapproval prompted: %s", progress.String())
	}
	canonical, err := filepath.EvalSymlinks(target)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, event := range readEvents(t, trace) {
		if event.Type == "tool.started" && event.Data.(map[string]any)["call_id"] == "pwd" {
			found = event.Data.(map[string]any)["workspace"] == canonical
		}
	}
	if !found {
		t.Fatal("CLI exec kept launch cwd")
	}
	for _, path := range []string{"relative", "/", filepath.Join(target, "missing", "child")} {
		out.Reset()
		progress.Reset()
		if code := Main(context.Background(), []string{"run", "--workspace", root, "--allow-workspace", path, "--show-context", "task"}, strings.NewReader(""), &out, &progress); code != exitUsage {
			t.Fatalf("invalid preapproval %q: %d", path, code)
		}
	}
	out.Reset()
	progress.Reset()
	if code := Main(context.Background(), []string{"run", "--workspace", root, "--allow-workspace", target, "--show-context", "task"}, strings.NewReader(""), &out, &progress); code != exitOK || !strings.Contains(out.String(), "switch_workspace") {
		t.Fatalf("preview %d %s", code, progress.String())
	}
}
