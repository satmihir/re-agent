package reagent

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func workspaceSession(t *testing.T, root string, mode Mode, model Model) *Session {
	t.Helper()
	ws, err := OpenWorkspace(root)
	if err != nil {
		t.Fatal(err)
	}
	registry, err := NewRegistry(mode, NewListFilesTool(ws), NewReadFileTool(ws), NewSearchTextTool(ws),
		NewEditFileTool(ws), NewWriteFileTool(ws), NewDeleteFileTool(ws), NewExecTool(ws),
		NewRequestWorkspaceAccessTool(), NewSwitchWorkspaceTool())
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Model: "test", Registry: registry, Workspace: ws, WorkspacePath: ws.Root(),
		ProjectInstructions: loadProjectInstructions(ws, false, io.Discard), MaxSteps: 30, MaxToolCalls: 50}
	s := NewSession(cfg, model, NewTrace(io.Discard), io.Discard)
	s.snapshot = collectSnapshot
	return s
}

func workspaceTool(t *testing.T, s *Session, name string, args any) ToolOutcome {
	t.Helper()
	tool, found := s.cfg.Registry.Lookup(name)
	if !found {
		t.Fatalf("missing tool %s", name)
	}
	return runTool(t, tool, string(mustJSON(t, args)))
}

func workspaceGit(t *testing.T, root string, args ...string) {
	t.Helper()
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git unavailable")
	}
	command := exec.Command("git", append([]string{"-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false", "-c", "user.name=Test", "-c", "user.email=test@invalid"}, args...)...)
	command.Dir, command.Env = root, childEnvironment()
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, output)
	}
}

func TestWorkspaceSwitch_CreateWorktreeThenUseEveryTool(t *testing.T) {
	parent := t.TempDir()
	root, target := filepath.Join(parent, "launch"), filepath.Join(parent, "worktree with spaces")
	if err := os.Mkdir(root, 0o700); err != nil {
		t.Fatal(err)
	}
	workspaceGit(t, root, "init", "--template="+t.TempDir(), "-b", "main")
	for name, text := range map[string]string{"shared.txt": "launch\n", "AGENTS.md": "launch instructions\n"} {
		if err := os.WriteFile(filepath.Join(root, name), []byte(text), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	workspaceGit(t, root, "add", ".")
	workspaceGit(t, root, "commit", "-m", "launch")
	workspaceGit(t, root, "checkout", "-b", "target")
	for name, text := range map[string]string{"shared.txt": "target\n", "AGENTS.md": "target instructions\n"} {
		if err := os.WriteFile(filepath.Join(root, name), []byte(text), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	workspaceGit(t, root, "commit", "-am", "target")
	workspaceGit(t, root, "checkout", "main")
	call := func(id, name string, args any) ModelResponse {
		return turn(callBlock(id, name, string(mustJSON(t, args))))
	}
	path := map[string]string{"path": target}
	model := &compactModel{replies: []ModelResponse{
		call("approve", "request_workspace_access", path),
		call("create", "exec", map[string]any{"argv": []string{"git", "-c", "core.hooksPath=/dev/null", "worktree", "add", "--detach", target, "target"}, "cwd": "."}),
		call("switch", "switch_workspace", path),
		call("read", "read_file", map[string]string{"path": "shared.txt"}),
		turn(callBlock("list", "list_files", `{"path":"."}`), callBlock("search", "search_text", `{"path":".","query":"target"}`)),
		call("edit", "edit_file", map[string]string{"path": "shared.txt", "expected_sha256": digestOf("target\n"), "old_text": "target", "new_text": "edited"}),
		call("write", "write_file", map[string]string{"path": "new.txt", "content": "new\n"}),
		call("overwrite", "write_file", map[string]string{"path": "new.txt", "content": "replacement\n", "expected_sha256": digestOf("new\n")}),
		call("delete", "delete_file", map[string]string{"path": "new.txt", "expected_sha256": digestOf("replacement\n")}),
		call("exec", "exec", map[string]any{"argv": []string{"pwd"}, "cwd": "."}),
		turn(textBlock("done")),
	}}
	s := workspaceSession(t, root, Mode{}, model)
	approved := 0
	s.workspaceConsent = func(_ context.Context, destination workspaceDestination) (bool, error) {
		approved++
		if !destination.missing {
			t.Fatal("creation destination was not prospective")
		}
		return true, nil
	}
	tracePath := filepath.Join(t.TempDir(), "events.jsonl")
	result, err := s.Turn(context.Background(), "create and work there", "run", tracePath)
	if err != nil || result.Status != StatusCompleted || approved != 1 {
		t.Fatalf("%+v, %v; prompts %d", result, err, approved)
	}
	canonical, err := filepath.EvalSymlinks(target)
	if err != nil {
		t.Fatal(err)
	}
	if s.cfg.WorkspacePath != canonical {
		t.Fatalf("active %q", s.cfg.WorkspacePath)
	}
	original, _ := os.ReadFile(filepath.Join(root, "shared.txt"))
	edited, _ := os.ReadFile(filepath.Join(target, "shared.txt"))
	if string(original) != "launch\n" || string(edited) != "edited\n" {
		t.Fatalf("launch %q target %q", original, edited)
	}
	if _, err := os.Stat(filepath.Join(target, "new.txt")); !os.IsNotExist(err) {
		t.Fatal("delete did not remove new.txt")
	}
	info, err := os.Stat(filepath.Join(target, ".git"))
	if err != nil || !info.Mode().IsRegular() {
		t.Fatal("fixture is not a linked worktree")
	}
	observations := results(s)
	for _, observation := range observations {
		if !observation.Outcome.OK {
			t.Fatalf("%s: %+v", observation.Name, observation.Outcome)
		}
		if encodedSize(observation.Outcome) > MaxResultBytes {
			t.Fatal("oversized observation")
		}
	}
	var read readFileResult
	data(t, observations[3].Outcome, &read)
	if read.Lines[0].Text != "target" || observations[3].Outcome.Workspace != canonical {
		t.Fatalf("read %+v", read)
	}
	var listing listFilesResult
	data(t, observations[4].Outcome, &listing)
	for _, entry := range listing.Entries {
		if entry.Name == ".git" {
			t.Fatal(".git file was exposed")
		}
	}
	var execution execResult
	data(t, observations[len(observations)-1].Outcome, &execution)
	if strings.TrimSpace(execution.Stdout) != canonical {
		t.Fatalf("exec ran at %q", execution.Stdout)
	}
	initialSpecs := string(mustJSON(t, model.requests[0].Tools))
	for i, req := range model.requests {
		if string(mustJSON(t, req.Tools)) != initialSpecs {
			t.Fatal("schemas changed")
		}
		if i >= 3 && (!strings.Contains(req.Instructions, "Workspace: "+canonical) || !strings.Contains(req.Instructions, "target instructions") || strings.Contains(req.Instructions, "launch instructions")) {
			t.Fatalf("stale prefix at request %d", i)
		}
	}
	var switched workspaceSwitchResult
	data(t, observations[2].Outcome, &switched)
	var state workspaceState
	if err := json.Unmarshal(switched.State, &state); err != nil {
		t.Fatal(err)
	}
	if state.Workspace != canonical || state.Git == nil || state.Git.Branch != "(detached)" {
		t.Fatalf("snapshot %+v", state)
	}
	for _, effect := range result.Effects {
		if effect.CallID != "create" && effect.Workspace != canonical {
			t.Fatalf("unattributed effect %+v", effect)
		}
	}
	found := false
	for _, event := range readEvents(t, tracePath) {
		if event.Type == "tool.started" && event.Data.(map[string]any)["call_id"] == "exec" {
			found = event.Data.(map[string]any)["workspace"] == canonical
		}
	}
	if !found {
		t.Fatal("trace retained launch location")
	}
	// Earlier requests keep their original prefix and user snapshot.
	if !strings.Contains(model.requests[0].Instructions, "launch instructions") {
		t.Fatal("old request mutated")
	}
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": root}); !outcome.OK {
		t.Fatal(outcome)
	}
	var launchRead readFileResult
	data(t, workspaceTool(t, s, "read_file", map[string]string{"path": "shared.txt"}), &launchRead)
	if launchRead.Lines[0].Text != "launch" || approved != 1 {
		t.Fatal("switching back used wrong root or prompted")
	}
}

func TestWorkspaceSwitch_PagingAndExecClippingStayIndependent(t *testing.T) {
	root, target := t.TempDir(), t.TempDir()
	text := strings.Repeat("a full line of evidence\n", 5000)
	if err := os.WriteFile(filepath.Join(target, "large.txt"), []byte(text), 0o600); err != nil {
		t.Fatal(err)
	}
	s := workspaceSession(t, root, Mode{}, NewScriptedModel())
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	var reconstructed strings.Builder
	pages, start := 0, 1
	for {
		outcome := workspaceTool(t, s, "read_file", map[string]any{"path": "large.txt", "start_line": start})
		if encodedSize(outcome) > MaxResultBytes {
			t.Fatal("page too large")
		}
		var page readFileResult
		data(t, outcome, &page)
		for _, line := range page.Lines {
			reconstructed.WriteString(line.Text + "\n")
		}
		pages++
		if page.SHA256 != digestOf(text) {
			t.Fatal("inconsistent digest")
		}
		if page.EOF {
			break
		}
		if page.NextLine == nil || *page.NextLine <= start {
			t.Fatal("paging did not advance")
		}
		start = *page.NextLine
	}
	if pages < 2 || reconstructed.String() != text {
		t.Fatalf("pages %d, bytes %d", pages, reconstructed.Len())
	}
	outcome := workspaceTool(t, s, "exec", map[string]any{"argv": []string{"sh", "-c", "printf '%050000d' 0"}, "cwd": "."})
	var execution execResult
	data(t, outcome, &execution)
	if !outcome.Truncated || !execution.StdoutTruncated || encodedSize(outcome) > MaxResultBytes {
		t.Fatal("exec clipping was not honest")
	}
	if err := os.WriteFile(filepath.Join(target, "long.txt"), []byte(strings.Repeat("x", MaxResultBytes)), 0o600); err != nil {
		t.Fatal(err)
	}
	if outcome := workspaceTool(t, s, "read_file", map[string]string{"path": "long.txt"}); outcome.Code != "line_too_long" {
		t.Fatal(outcome)
	}
	if err := os.WriteFile(filepath.Join(target, "huge.txt"), []byte(strings.Repeat("x", MaxFileBytes+1)), 0o600); err != nil {
		t.Fatal(err)
	}
	if outcome := workspaceTool(t, s, "read_file", map[string]string{"path": "huge.txt"}); outcome.Code != "file_too_large" {
		t.Fatal(outcome)
	}
}

func TestWorkspaceSwitch_MixedBatchDoesNothing(t *testing.T) {
	for _, name := range []string{"switch_workspace", "request_workspace_access"} {
		t.Run(name, func(t *testing.T) {
			root, target := t.TempDir(), t.TempDir()
			model := NewScriptedModel(turn(callBlock("write", "write_file", `{"path":"oops","content":"oops"}`), callBlock("move", name, string(mustJSON(t, map[string]string{"path": target})))), turn(textBlock("corrected")))
			s := workspaceSession(t, root, Mode{}, model)
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) {
				t.Fatal("picker opened for mixed batch")
				return true, nil
			}
			result, err := s.Turn(context.Background(), "task", "run", filepath.Join(t.TempDir(), "events.jsonl"))
			if err != nil || result.Status != StatusCompleted || result.ToolCalls != 2 {
				t.Fatalf("%+v %v", result, err)
			}
			for _, observation := range results(s) {
				if observation.Outcome.Code != "not_executed" || observation.Outcome.Workspace != s.cfg.WorkspacePath {
					t.Fatal(observation)
				}
			}
			if _, err := os.Stat(filepath.Join(root, "oops")); !os.IsNotExist(err) {
				t.Fatal("mixed batch wrote a file")
			}
			for _, event := range readEvents(t, result.TracePath) {
				if event.Type == "tool.started" {
					t.Fatal("mixed batch started a tool")
				}
			}
		})
	}
}
