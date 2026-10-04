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

func TestWorkspaceTools_ArgumentAndDestinationFailuresAreAtomic(t *testing.T) {
	root, target := t.TempDir(), t.TempDir()
	file := filepath.Join(target, "file")
	if err := os.WriteFile(file, []byte("not a directory"), 0o600); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"request_workspace_access", "switch_workspace"} {
		for _, tc := range []struct{ label, args, code string }{
			{"omitted", `{}`, "invalid_arguments"}, {"null", `{"path":null}`, "invalid_arguments"},
			{"number", `{"path":3}`, "invalid_arguments"}, {"unknown", `{"path":"/tmp","approve":true}`, "invalid_arguments"},
			{"blank", `{"path":"  "}`, "invalid_arguments"}, {"relative", `{"path":"relative"}`, "invalid_path"},
			{"nul", `{"path":"/x\u0000y"}`, "invalid_path"}, {"root", `{"path":"/"}`, "invalid_path"},
			{"traversal", string(mustJSON(t, map[string]string{"path": target + "/../other"})), "invalid_path"},
			{"withheld", string(mustJSON(t, map[string]string{"path": filepath.Join(target, ".git")})), "invalid_path"},
			{"file", string(mustJSON(t, map[string]string{"path": file})), "not_directory"},
			{"missing parent", string(mustJSON(t, map[string]string{"path": filepath.Join(target, "missing", "child")})), "not_found"},
			{"unapproved", string(mustJSON(t, map[string]string{"path": target})), "permission_required"},
		} {
			t.Run(name+"/"+tc.label, func(t *testing.T) {
				s := workspaceSession(t, root, Mode{}, NewScriptedModel())
				before := s.cfg
				tool, _ := s.cfg.Registry.Lookup(name)
				outcome := runTool(t, tool, tc.args)
				if outcome.Code != tc.code || outcome.Effect != EffectNone {
					t.Fatalf("%+v", outcome)
				}
				if s.cfg.WorkspacePath != before.WorkspacePath || s.cfg.Registry != before.Registry || s.cfg.ProjectInstructions != before.ProjectInstructions || len(s.workspace.approved) != 1 || len(s.history) != 0 {
					t.Fatal("failure changed selection, history, or authority")
				}
			})
		}
	}
}

func TestWorkspaceTools_ProspectiveConsentDoesNotCreateOrExposeParent(t *testing.T) {
	root, parent := t.TempDir(), t.TempDir()
	target := filepath.Join(parent, "future")
	s := workspaceSession(t, root, Mode{}, NewScriptedModel())
	prompts := 0
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { prompts++; return true, nil }
	if outcome := workspaceTool(t, s, "request_workspace_access", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	if _, err := os.Stat(target); !os.IsNotExist(err) {
		t.Fatal("access request created a directory")
	}
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); outcome.Code != "not_found" {
		t.Fatal(outcome)
	}
	// A parent and a sibling require their own consent.
	s.workspaceConsent = nil
	for _, path := range []string{parent, filepath.Join(parent, "sibling")} {
		if outcome := workspaceTool(t, s, "request_workspace_access", map[string]string{"path": path}); outcome.Code != "permission_required" {
			t.Fatal(outcome)
		}
	}
	if err := os.Mkdir(target, 0o700); err != nil {
		t.Fatal(err)
	}
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	if prompts != 1 {
		t.Fatalf("prompts %d", prompts)
	}
}

func TestWorkspaceTools_ReservedDestinationCannotBeRedirected(t *testing.T) {
	for _, redirect := range []string{"symlink leaf", "replaced parent", "symlink parent"} {
		t.Run(redirect, func(t *testing.T) {
			root, parent, outside := t.TempDir(), t.TempDir(), t.TempDir()
			target := filepath.Join(parent, "future")
			s := workspaceSession(t, root, Mode{}, NewScriptedModel())
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
			if outcome := workspaceTool(t, s, "request_workspace_access", map[string]string{"path": target}); !outcome.OK {
				t.Fatal(outcome)
			}
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) {
				t.Fatal("invalidated grant opened a picker")
				return true, nil
			}
			switch redirect {
			case "symlink leaf":
				if err := os.Symlink(outside, target); err != nil {
					t.Skipf("symlinks: %v", err)
				}
			case "replaced parent":
				if err := os.Rename(parent, parent+"-old"); err != nil {
					t.Fatal(err)
				}
				t.Cleanup(func() { os.RemoveAll(parent + "-old") })
				if err := os.MkdirAll(target, 0o700); err != nil {
					t.Fatal(err)
				}
			case "symlink parent":
				if err := os.Rename(parent, parent+"-old"); err != nil {
					t.Fatal(err)
				}
				t.Cleanup(func() { os.RemoveAll(parent + "-old") })
				if err := os.Mkdir(filepath.Join(outside, "future"), 0o700); err != nil {
					t.Fatal(err)
				}
				if err := os.Symlink(outside, parent); err != nil {
					t.Skipf("symlinks: %v", err)
				}
			}
			before := s.cfg.WorkspacePath
			outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target})
			if outcome.OK || s.cfg.WorkspacePath != before {
				t.Fatalf("redirect admitted: %+v", outcome)
			}
		})
	}
}

func TestWorkspaceSwitch_AliasesContainmentAndDigestIdentity(t *testing.T) {
	root, target, outside := t.TempDir(), t.TempDir(), t.TempDir()
	for dir, text := range map[string]string{root: "launch", target: "target", outside: "secret"} {
		if err := os.WriteFile(filepath.Join(dir, "a.txt"), []byte(text), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	alias := filepath.Join(t.TempDir(), "alias")
	if err := os.Symlink(target, alias); err != nil {
		t.Skipf("symlinks: %v", err)
	}
	s := workspaceSession(t, root, Mode{}, NewScriptedModel())
	prompts := 0
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { prompts++; return true, nil }
	var launch readFileResult
	data(t, workspaceTool(t, s, "read_file", map[string]string{"path": "a.txt"}), &launch)
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": alias}); !outcome.OK {
		t.Fatal(outcome)
	}
	if outcome := workspaceTool(t, s, "edit_file", map[string]string{"path": "a.txt", "expected_sha256": launch.SHA256, "old_text": "target", "new_text": "changed"}); outcome.Code != "unknown_digest" {
		t.Fatal(outcome)
	}
	var targetRead readFileResult
	data(t, workspaceTool(t, s, "read_file", map[string]string{"path": "a.txt"}), &targetRead)
	if err := os.WriteFile(filepath.Join(target, "a.txt"), []byte("external"), 0o600); err != nil {
		t.Fatal(err)
	}
	if outcome := workspaceTool(t, s, "edit_file", map[string]string{"path": "a.txt", "expected_sha256": targetRead.SHA256, "old_text": "external", "new_text": "changed"}); outcome.Code != "stale_file" {
		t.Fatal(outcome)
	}
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": root}); !outcome.OK {
		t.Fatal(outcome)
	}
	if err := os.WriteFile(filepath.Join(root, "a.txt"), []byte("externally changed launch"), 0o600); err != nil {
		t.Fatal(err)
	}
	if outcome := workspaceTool(t, s, "edit_file", map[string]string{"path": "a.txt", "expected_sha256": launch.SHA256, "old_text": "launch", "new_text": "changed"}); outcome.Code != "stale_file" {
		t.Fatal(outcome)
	}
	if prompts != 1 {
		t.Fatalf("canonical aliases prompted %d times", prompts)
	}
	if err := os.Symlink(outside, filepath.Join(root, "escape")); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, ".env"), []byte("secret"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(".env", filepath.Join(root, "env-alias")); err != nil {
		t.Fatal(err)
	}
	for _, path := range []string{"escape/a.txt", "env-alias"} {
		if outcome := workspaceTool(t, s, "read_file", map[string]string{"path": path}); outcome.Code != "invalid_path" {
			t.Fatal(outcome)
		}
	}
	if outcome := workspaceTool(t, s, "write_file", map[string]string{"path": "escape/new", "content": "bad"}); outcome.Code != "invalid_path" {
		t.Fatal(outcome)
	}
	if outcome := workspaceTool(t, s, "exec", map[string]any{"argv": []string{"pwd"}, "cwd": "escape"}); outcome.Code != "invalid_path" {
		t.Fatal(outcome)
	}
	if outcome := workspaceTool(t, s, "read_file", map[string]string{"path": filepath.Join(s.cfg.WorkspacePath, "a.txt")}); !outcome.OK {
		t.Fatal(outcome)
	}
	if err := os.Mkdir(filepath.Join(root, "nested"), 0o700); err != nil {
		t.Fatal(err)
	}
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": filepath.Join(root, "nested")}); !outcome.OK || prompts != 1 {
		t.Fatal(outcome)
	}
}

func TestWorkspaceSwitch_ProjectInstructionsReloadWithoutChangingOldRequests(t *testing.T) {
	for _, kind := range []string{"loaded", "missing", "oversized", "non-UTF8", "directory", "inside symlink", "outside symlink", "withheld symlink", "broken symlink", "disabled"} {
		t.Run(kind, func(t *testing.T) {
			root, target := t.TempDir(), t.TempDir()
			if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("old instructions"), 0o600); err != nil {
				t.Fatal(err)
			}
			path := filepath.Join(target, "AGENTS.md")
			text := "new instructions"
			switch kind {
			case "loaded", "disabled":
				if err := os.WriteFile(path, []byte(text), 0o600); err != nil {
					t.Fatal(err)
				}
			case "oversized":
				if err := os.WriteFile(path, []byte(strings.Repeat("x", (32<<10)+1)), 0o600); err != nil {
					t.Fatal(err)
				}
			case "non-UTF8":
				if err := os.WriteFile(path, []byte{0xff}, 0o600); err != nil {
					t.Fatal(err)
				}
			case "directory":
				if err := os.Mkdir(path, 0o700); err != nil {
					t.Fatal(err)
				}
			case "inside symlink", "outside symlink", "withheld symlink", "broken symlink":
				link := filepath.Join(target, "rules")
				if kind == "outside symlink" {
					link = filepath.Join(root, "AGENTS.md")
				}
				if kind == "withheld symlink" {
					link = filepath.Join(target, ".env")
				}
				if kind != "broken symlink" && kind != "outside symlink" {
					if err := os.WriteFile(link, []byte(text), 0o600); err != nil {
						t.Fatal(err)
					}
				}
				if err := os.Symlink(link, path); err != nil {
					t.Skipf("symlinks: %v", err)
				}
			}
			s := workspaceSession(t, root, Mode{}, NewScriptedModel())
			old := BuildContext(s.cfg, RequestScope{}, nil)
			s.cfg.NoProjectInstructions = kind == "disabled"
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
			if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
				t.Fatal(outcome)
			}
			current := BuildContext(s.cfg, RequestScope{}, nil)
			loaded := kind == "loaded" || kind == "inside symlink"
			if strings.Contains(current.Instructions, "old instructions") || strings.Contains(current.Instructions, text) != loaded || !strings.Contains(old.Instructions, "old instructions") {
				t.Fatal("effective project instructions are stale")
			}
			for _, encode := range []func(ModelRequest) ([]byte, error){EncodeOpenAIRequest, EncodeAnthropicRequest} {
				body, err := encode(current)
				if err != nil || !strings.Contains(string(body), s.cfg.WorkspacePath) {
					t.Fatalf("encoder %v", err)
				}
			}
			if err := os.RemoveAll(path); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(path, []byte("later instructions"), 0o600); err != nil {
				t.Fatal(err)
			}
			if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
				t.Fatal(outcome)
			}
			if strings.Contains(BuildContext(s.cfg, RequestScope{}, nil).Instructions, "later instructions") {
				t.Fatal("no-op reloaded instructions")
			}
		})
	}
}

func TestWorkspaceSwitch_IndependentSessionsDoNotShareSelectionOrDigests(t *testing.T) {
	root, target := t.TempDir(), t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "a.txt"), []byte("before"), 0o600); err != nil {
		t.Fatal(err)
	}
	first := workspaceSession(t, root, Mode{}, NewScriptedModel())
	second := NewSession(first.cfg, NewScriptedModel(), NewTrace(io.Discard), io.Discard)
	workspaceTool(t, first, "read_file", map[string]string{"path": "a.txt"})
	if err := os.WriteFile(filepath.Join(root, "a.txt"), []byte("after"), 0o600); err != nil {
		t.Fatal(err)
	}
	if outcome := workspaceTool(t, second, "edit_file", map[string]string{"path": "a.txt", "expected_sha256": digestOf("before"), "old_text": "after", "new_text": "new"}); outcome.Code != "unknown_digest" {
		t.Fatal(outcome)
	}
	if err := os.WriteFile(filepath.Join(target, "a.txt"), []byte("before"), 0o600); err != nil {
		t.Fatal(err)
	}
	first.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
	if outcome := workspaceTool(t, first, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	if outcome := workspaceTool(t, first, "edit_file", map[string]string{"path": "a.txt", "old_text": "before", "new_text": "new"}); outcome.Code != "invalid_arguments" || !strings.Contains(outcome.Message, "read the file before changing it") {
		t.Fatalf("new workspace inherited seen: %+v", outcome)
	}
	if second.cfg.WorkspacePath == first.cfg.WorkspacePath || len(second.workspace.approved) != 1 {
		t.Fatal("sessions shared mutable authority or root")
	}
	// A shell's own cd never selects a workspace for the next call.
	before := first.cfg.WorkspacePath
	if outcome := workspaceTool(t, first, "exec", map[string]any{"argv": []string{"sh", "-c", "cd / && pwd"}, "cwd": "."}); !outcome.OK || first.cfg.WorkspacePath != before {
		t.Fatal(outcome)
	}
}

func TestWorkspaceSwitch_InvalidLinkedWorktreeDoesNotAskForConsent(t *testing.T) {
	root, target := t.TempDir(), t.TempDir()
	if err := os.WriteFile(filepath.Join(target, ".git"), []byte("gitdir: missing"), 0o600); err != nil {
		t.Fatal(err)
	}
	s := workspaceSession(t, root, Mode{}, NewScriptedModel())
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) {
		t.Fatal("invalid target requested consent")
		return true, nil
	}
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); outcome.Code != "workspace_validation_failed" {
		t.Fatal(outcome)
	}
}

func TestWorkspaceSwitch_ContextReadClassificationIncludesRoot(t *testing.T) {
	root, target := t.TempDir(), t.TempDir()
	for _, path := range []string{root, target} {
		if err := os.WriteFile(filepath.Join(path, "a.txt"), []byte("same"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	s := workspaceSession(t, root, Mode{}, NewScriptedModel())
	read := workspaceTool(t, s, "read_file", map[string]string{"path": "a.txt"})
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
	workspaceTool(t, s, "switch_workspace", map[string]string{"path": target})
	edit := workspaceTool(t, s, "edit_file", map[string]string{"path": "a.txt", "expected_sha256": digestOf("same"), "old_text": "same", "new_text": "new"})
	history := []Entry{{Kind: EntryTool, Tool: &ToolResult{Name: "read_file", Outcome: read}}, {Kind: EntryTool, Tool: &ToolResult{Name: "edit_file", Outcome: edit}}}
	cfg := s.cfg
	cfg.Provider = openaiName
	breakdown, err := measureContext(cfg, history)
	if err != nil {
		t.Fatal(err)
	}
	for _, part := range breakdown.parts {
		if part.label == "file edited since" && part.bytes != 0 {
			t.Fatal("edit in another root marked read stale")
		}
	}
	before, _ := json.Marshal(history)
	BuildContext(cfg, RequestScope{}, history)
	after, _ := json.Marshal(history)
	if string(before) != string(after) {
		t.Fatal("context mutated history")
	}
}

func TestWorkspaceTools_PrefixLookalikeIsNotApproved(t *testing.T) {
	root, parent := t.TempDir(), t.TempDir()
	target, sibling := filepath.Join(parent, "approved"), filepath.Join(parent, "approved-sibling")
	for _, path := range []string{target, sibling} {
		if err := os.Mkdir(path, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	s := workspaceSession(t, root, Mode{}, NewScriptedModel())
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
	if outcome := workspaceTool(t, s, "request_workspace_access", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	s.workspaceConsent = nil
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": sibling}); outcome.Code != "permission_required" {
		t.Fatal(outcome)
	}
}
