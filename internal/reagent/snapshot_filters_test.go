package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestSnapshot_CleanFilterCannotRunInRestrictedSwitchedWorkspace(t *testing.T) {
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
			script := filepath.Join(root, "clean.sh")
			marker := script + ".marker"
			if err := os.WriteFile(script, []byte("#!/bin/sh\n: > \"$0.marker\"\ncat\n"), 0o700); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(filepath.Join(target, ".gitattributes"), []byte("f.txt filter=evil\n"), 0o600); err != nil {
				t.Fatal(err)
			}
			file := filepath.Join(target, "f.txt")
			if err := os.WriteFile(file, []byte("data\n"), 0o600); err != nil {
				t.Fatal(err)
			}
			workspaceGit(t, target, "config", "filter.evil.clean", script)
			workspaceGit(t, target, "add", ".")
			workspaceGit(t, target, "commit", "-m", "fixture")
			workspaceGit(t, target, "remote", "add", "origin", ".")
			workspaceGit(t, target, "update-ref", "refs/remotes/origin/main", "HEAD")
			workspaceGit(t, target, "branch", "--set-upstream-to=origin/main", "main")
			changed := time.Now().Add(2 * time.Second)
			if err := os.Chtimes(file, changed, changed); err != nil {
				t.Fatal(err)
			}
			if err := os.Remove(marker); err != nil {
				t.Fatal(err)
			}
			// Prove status with fsmonitor disabled still invokes the clean filter.
			collectSnapshot(context.Background(), target, false)
			if _, err := os.Stat(marker); err != nil {
				t.Fatal("fixture did not demonstrate clean-filter execution")
			}
			if err := os.Remove(marker); err != nil {
				t.Fatal(err)
			}
			// Dirty only the stat data again: file contents and size still match the index.
			changed = changed.Add(time.Second)
			if err := os.Chtimes(file, changed, changed); err != nil {
				t.Fatal(err)
			}
			model := &compactModel{replies: []ModelResponse{
				turn(callBlock("switch", "switch_workspace", string(mustJSON(t, map[string]string{"path": target})))),
				turn(textBlock("switched")), turn(textBlock("next turn")),
			}}
			s := workspaceSession(t, root, tc.mode, model)
			s.planMode = tc.plan
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) {
				t.Fatal("descendant requested consent")
				return false, nil
			}
			result, err := s.Turn(context.Background(), "switch there", "switch", filepath.Join(t.TempDir(), "events.jsonl"))
			if err != nil || result.Status != StatusCompleted {
				t.Fatalf("%+v %v", result, err)
			}
			if _, err := os.Stat(marker); !os.IsNotExist(err) {
				t.Fatal("switch snapshot ran clean filter")
			}
			var switched workspaceSwitchResult
			data(t, results(s)[0].Outcome, &switched)
			assertRefOnlySnapshot(t, switched.State, "main")
			result, err = s.Turn(context.Background(), "continue", "next", filepath.Join(t.TempDir(), "events.jsonl"))
			if err != nil || result.Status != StatusCompleted {
				t.Fatalf("%+v %v", result, err)
			}
			if _, err := os.Stat(marker); !os.IsNotExist(err) {
				t.Fatal("next turn snapshot ran clean filter")
			}
			assertRefOnlySnapshot(t, model.requests[2].History[len(model.requests[2].History)-1].User.Workspace, "main")
			// Recovery paths must not forget which root was selected at launch.
			s.Reset()
			c := &conversation{session: s, trace: s.trace, client: NewHTTPClient(), progress: io.Discard}
			info, _ := findModel("claude-haiku-4-5")
			c.switchTo(info)
			c.session.model = NewScriptedModel(turn(textBlock("after model change")))
			if result, err := c.session.Turn(context.Background(), "continue", "replacement", filepath.Join(t.TempDir(), "events.jsonl")); err != nil || result.Status != StatusCompleted {
				t.Fatalf("%+v %v", result, err)
			}
			if _, err := os.Stat(marker); !os.IsNotExist(err) {
				t.Fatal("reset/model replacement forgot restricted snapshot policy")
			}
		})
	}
}

func assertRefOnlySnapshot(t *testing.T, raw json.RawMessage, branch string) {
	t.Helper()
	var snapshot struct {
		Counts string                     `json:"counts"`
		Git    map[string]json.RawMessage `json:"git"`
	}
	if err := json.Unmarshal(raw, &snapshot); err != nil {
		t.Fatal(err)
	}
	if snapshot.Counts != "omitted in read-only/plan mode" {
		t.Fatalf("unmarked omission: %s", raw)
	}
	for _, name := range []string{"staged", "modified", "untracked"} {
		if _, present := snapshot.Git[name]; present {
			t.Fatalf("unknown counts looked clean: %s", raw)
		}
	}
	var observed string
	if err := json.Unmarshal(snapshot.Git["branch"], &observed); err != nil || observed != branch {
		t.Fatalf("branch lost: %s", raw)
	}
	if err := json.Unmarshal(snapshot.Git["upstream"], &observed); err != nil || observed != "origin/main" {
		t.Fatalf("upstream lost: %s", raw)
	}
	for _, name := range []string{"ahead", "behind", "ahead_of_default", "behind_default"} {
		var count int
		if err := json.Unmarshal(snapshot.Git[name], &count); err != nil || count != 0 {
			t.Fatalf("ref divergence lost: %s", raw)
		}
	}
}

func TestSnapshot_RefOnlyUnbornDetachedAndUnavailableGit(t *testing.T) {
	root := t.TempDir()
	workspaceGit(t, root, "init", "--template="+t.TempDir(), "-b", "main")
	var state workspaceState
	decode := func(root string, refsOnly bool) workspaceState {
		var snapshot workspaceState
		if err := json.Unmarshal(collectSnapshot(context.Background(), root, refsOnly), &snapshot); err != nil {
			t.Fatal(err)
		}
		return snapshot
	}
	state = decode(root, true)
	if state.Git == nil || state.Git.Branch != "main" || state.Git.Staged != nil || state.Counts == "" {
		t.Fatalf("unborn: %+v", state)
	}
	state = decode(root, false)
	if state.Counts != "" || state.Git == nil || state.Git.Staged == nil || *state.Git.Staged != 0 || state.Git.Modified == nil || *state.Git.Modified != 0 || state.Git.Untracked == nil || *state.Git.Untracked != 0 {
		t.Fatalf("known zero counts omitted: %+v", state)
	}
	if err := os.WriteFile(filepath.Join(root, "f"), []byte("text"), 0o600); err != nil {
		t.Fatal(err)
	}
	workspaceGit(t, root, "add", ".")
	workspaceGit(t, root, "commit", "-m", "fixture")
	workspaceGit(t, root, "checkout", "--detach")
	state = decode(root, true)
	if state.Git == nil || state.Git.Branch != "(detached)" || state.Git.Untracked != nil || state.Git.Upstream != "" {
		t.Fatalf("detached: %+v", state)
	}
	state = decode(t.TempDir(), true)
	if state.Git != nil || state.Counts != "omitted in read-only/plan mode" {
		t.Fatalf("unavailable Git looked clean: %+v", state)
	}
}

func TestSnapshot_PolicyTracksLaunchRootAndPlanToggles(t *testing.T) {
	t.Parallel()
	for _, readOnly := range []bool{false, true} {
		t.Run(fmt.Sprint(readOnly), func(t *testing.T) {
			t.Parallel()
			root, target := t.TempDir(), t.TempDir()
			for _, path := range []string{root, target} {
				workspaceGit(t, path, "init", "--template="+t.TempDir(), "-b", "main")
			}
			model := &compactModel{replies: []ModelResponse{turn(textBlock("launch")), turn(textBlock("selected")), turn(textBlock("planning")), turn(textBlock("plan ended")), turn(textBlock("returned"))}}
			s := workspaceSession(t, root, Mode{ReadOnly: readOnly}, model)
			s.planMode = true
			turnNumber := 0
			turnSnapshot := func() workspaceState {
				turnNumber++
				if result, err := s.Turn(context.Background(), "turn", NewID(), filepath.Join(t.TempDir(), "events.jsonl")); err != nil || result.Status != StatusCompleted {
					t.Fatalf("%+v %v", result, err)
				}
				var state workspaceState
				if err := json.Unmarshal(model.requests[turnNumber-1].History[len(model.requests[turnNumber-1].History)-1].User.Workspace, &state); err != nil {
					t.Fatal(err)
				}
				return state
			}
			if state := turnSnapshot(); state.Counts != "" || state.Git.Staged == nil {
				t.Fatal("launch-root exception changed")
			}
			s.planMode = false
			s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
			if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
				t.Fatal(outcome)
			}
			if state := turnSnapshot(); (state.Counts != "") != readOnly {
				t.Fatal("unrestricted selected-root counts changed")
			}
			s.planMode = true
			if state := turnSnapshot(); state.Counts == "" || state.Git.Staged != nil {
				t.Fatal("plan toggle did not restrict snapshot")
			}
			s.planMode = false
			if state := turnSnapshot(); (state.Counts != "") != readOnly {
				t.Fatal("plan ended marker incorrectly restored launch authority")
			}
			if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": root}); !outcome.OK {
				t.Fatal(outcome)
			}
			if state := turnSnapshot(); state.Counts != "" || state.Git.Staged == nil {
				t.Fatal("return to launch did not restore full snapshot")
			}
		})
	}
}
