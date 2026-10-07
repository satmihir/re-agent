package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestWorkspaceReview_WithheldNamesIgnoreCase(t *testing.T) {
	ws := testWorkspace(t, map[string]string{
		".GIT/config": "protected", ".ENV": "protected", ".Env.local": "protected", "visible": "public",
	})
	s := workspaceSession(t, ws.Root(), Mode{}, NewScriptedModel())
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) {
		t.Fatal("withheld location requested consent")
		return true, nil
	}
	for _, path := range []string{".GIT/config", ".ENV", ".Env.local"} {
		for _, tc := range []struct {
			tool string
			args any
		}{
			{"read_file", map[string]string{"path": path}},
			{"search_text", map[string]string{"path": path, "query": "protected"}},
			{"write_file", map[string]string{"path": path, "content": "bad", "expected_sha256": digestOf("protected")}},
			{"edit_file", map[string]string{"path": path, "old_text": "protected", "new_text": "bad", "expected_sha256": digestOf("protected")}},
			{"delete_file", map[string]string{"path": path, "expected_sha256": digestOf("protected")}},
		} {
			t.Run(tc.tool+"/"+path, func(t *testing.T) {
				if outcome := workspaceTool(t, s, tc.tool, tc.args); outcome.Code != "invalid_path" {
					t.Fatal(outcome)
				}
			})
		}
	}
	for _, name := range []string{"switch_workspace", "request_workspace_access"} {
		path := filepath.Join(ws.Root(), ".GIT")
		if name == "request_workspace_access" {
			path = filepath.Join(ws.Root(), ".Git", "newdir")
		}
		if outcome := workspaceTool(t, s, name, map[string]string{"path": path}); outcome.Code != "invalid_path" {
			t.Fatal(outcome)
		}
	}
	var listing listFilesResult
	data(t, workspaceTool(t, s, "list_files", map[string]string{"path": "."}), &listing)
	if len(listing.Entries) != 1 || listing.Entries[0].Name != "visible" {
		t.Fatalf("withheld entries exposed: %+v", listing)
	}
	var search searchTextResult
	data(t, workspaceTool(t, s, "search_text", map[string]string{"path": ".", "query": "protected"}), &search)
	if len(search.Matches) != 0 {
		t.Fatal("recursive search exposed mixed-case withheld files")
	}
	if outcome := workspaceTool(t, s, "exec", map[string]any{"argv": []string{"pwd"}, "cwd": ".GIT"}); outcome.Code != "invalid_path" {
		t.Fatal(outcome)
	}
}

func TestWorkspaceReview_SwitchDoesNotRunFSMonitor(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		mode Mode
		plan bool
	}{
		{"read-only", Mode{ReadOnly: true}, false}, {"plan", Mode{}, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			root := t.TempDir()
			target := filepath.Join(root, "vendor", "lib")
			if err := os.MkdirAll(target, 0o700); err != nil {
				t.Fatal(err)
			}
			workspaceGit(t, target, "init", "--template="+t.TempDir(), "-b", "main")
			script := filepath.Join(root, "monitor.sh")
			marker := script + ".marker"
			if err := os.WriteFile(script, []byte("#!/bin/sh\n: > \"$0.marker\"\nexit 1\n"), 0o700); err != nil {
				t.Fatal(err)
			}
			workspaceGit(t, target, "config", "core.fsmonitor", script)
			// Prove the local fixture can execute its configured monitor before testing suppression.
			workspaceGit(t, target, "status", "--porcelain=v2")
			if _, err := os.Stat(marker); err != nil {
				t.Fatalf("fsmonitor fixture did not execute: %v", err)
			}
			if err := os.Remove(marker); err != nil {
				t.Fatal(err)
			}
			s := workspaceSession(t, root, tc.mode, NewScriptedModel())
			s.planMode = tc.plan
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) {
				t.Fatal("descendant needs no consent")
				return false, nil
			}
			if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
				t.Fatal(outcome)
			}
			var state workspaceState
			if err := json.Unmarshal(collectSnapshot(context.Background(), s.cfg.WorkspacePath, false), &state); err != nil {
				t.Fatal(err)
			}
			if state.Git == nil {
				t.Fatal("suppression lost ordinary Git observations")
			}
			if _, err := os.Stat(marker); !os.IsNotExist(err) {
				t.Fatal("snapshot executed repository-configured fsmonitor")
			}
		})
	}
}

func TestWorkspaceReview_CaseVariantLaunchWorktreeCanBeReselected(t *testing.T) {
	parent := t.TempDir()
	repo, root, alias, sibling := filepath.Join(parent, "repo"), filepath.Join(parent, "Wt"), filepath.Join(parent, "wt"), filepath.Join(parent, "B")
	for _, path := range []string{repo, sibling} {
		if err := os.Mkdir(path, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	workspaceGit(t, repo, "init", "--template="+t.TempDir(), "-b", "main")
	if err := os.WriteFile(filepath.Join(repo, "a.txt"), []byte("worktree"), 0o600); err != nil {
		t.Fatal(err)
	}
	workspaceGit(t, repo, "add", ".")
	workspaceGit(t, repo, "commit", "-m", "fixture")
	workspaceGit(t, repo, "worktree", "add", "--detach", root)
	original, err := os.Stat(root)
	if err != nil {
		t.Fatal(err)
	}
	otherCase, err := os.Stat(alias)
	if err != nil || !os.SameFile(original, otherCase) {
		t.Skip("case-insensitive filesystem unavailable")
	}
	s := workspaceSession(t, alias, Mode{}, NewScriptedModel())
	launch := s.cfg.WorkspacePath
	prompts := 0
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { prompts++; return true, nil }
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": sibling}); !outcome.OK {
		t.Fatal(outcome)
	}
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": alias}); !outcome.OK {
		t.Fatal(outcome)
	}
	if prompts != 1 || s.cfg.WorkspacePath != launch {
		t.Fatal("case-variant launch root lost its approval")
	}
	var read readFileResult
	data(t, workspaceTool(t, s, "read_file", map[string]string{"path": "a.txt"}), &read)
	if read.Lines[0].Text != "worktree" {
		t.Fatal(read)
	}
}

type workspaceProbeTool struct{ ws *Workspace }

func (workspaceProbeTool) Spec() ToolSpec {
	return ToolSpec{Name: "workspace_probe", InputSchema: json.RawMessage(`{"type":"object","properties":{}}`), Effect: EffectClassRead}
}

func (t workspaceProbeTool) Execute(context.Context, json.RawMessage) (ToolOutcome, error) {
	return workspaceOutcome(struct {
		Root string `json:"root"`
	}{t.ws.Root()}, t.ws.Root())
}

func (t workspaceProbeTool) withWorkspace(ws *Workspace) Tool { t.ws = ws; return t }

func TestWorkspaceReview_RebindUsesCapability(t *testing.T) {
	ws := testWorkspace(t, nil)
	target := t.TempDir()
	registry, err := NewRegistry(Mode{}, NewSwitchWorkspaceTool(), workspaceProbeTool{ws})
	if err != nil {
		t.Fatal(err)
	}
	s := NewSession(Config{Workspace: ws, Registry: registry}, NewScriptedModel(), NewTrace(io.Discard), io.Discard)
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	var result struct {
		Root string `json:"root"`
	}
	data(t, workspaceTool(t, s, "workspace_probe", struct{}{}), &result)
	if result.Root != s.cfg.WorkspacePath {
		t.Fatal("tool not known to the registry's concrete type switch retained its launch root")
	}
	if !strings.Contains(string(mustJSON(t, s.cfg.Registry.Specs())), "workspace_probe") {
		t.Fatal("binding changed declarations")
	}
}

func TestWorkspaceReview_PipedConsentDoesNotPrintDisclosure(t *testing.T) {
	var progress bytes.Buffer
	input := newLineReader(strings.NewReader("next prompt\n"), &progress, nil)
	cfg := testConfig(t)
	approved, err := workspacePermission(context.Background(), input, &progress, cfg, false, workspaceDestination{path: t.TempDir()})
	if approved || err != errNotInteractive || progress.Len() != 0 {
		t.Fatalf("approved %t error %v output %q", approved, err, progress.String())
	}
	if line, err := input.ReadLine(); err != nil || line != "next prompt" {
		t.Fatal("noninteractive consent consumed prompt input")
	}
}

func TestWorkspaceReview_SwitchUpdatesExecutionNotice(t *testing.T) {
	for _, tc := range []struct {
		name string
		mode Mode
		plan bool
	}{{"exec", Mode{}, false}, {"read-only", Mode{ReadOnly: true}, false}, {"plan", Mode{}, true}} {
		t.Run(tc.name, func(t *testing.T) {
			s := workspaceSession(t, t.TempDir(), tc.mode, NewScriptedModel())
			s.planMode = tc.plan
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
			var progress bytes.Buffer
			s.display = NewDisplay(&progress)
			if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": t.TempDir()}); !outcome.OK {
				t.Fatal(outcome)
			}
			if tc.name == "exec" {
				if !strings.Contains(progress.String(), "commands run as you, in "+shortPath(s.cfg.WorkspacePath)) {
					t.Fatal("execution notice kept the launch root")
				}
			} else if strings.Contains(progress.String(), "commands run as you") {
				t.Fatal("notice implied exec authority in a restricted mode")
			}
		})
	}
}
