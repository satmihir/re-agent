package reagent

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func gitTest(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	cmd.Env = append(childEnvironment(), "GIT_AUTHOR_NAME=Test", "GIT_AUTHOR_EMAIL=test@example.com", "GIT_COMMITTER_NAME=Test", "GIT_COMMITTER_EMAIL=test@example.com")
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("git %v: %v: %s", args, err, out)
	}
}

func TestSnapshot_GitBranchAndChanges(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	dir := t.TempDir()
	gitTest(t, dir, "init", "-q", "-b", "main")
	gitTest(t, dir, "remote", "add", "origin", ".")
	if err := os.WriteFile(filepath.Join(dir, "tracked"), []byte("before"), 0600); err != nil {
		t.Fatal(err)
	}
	gitTest(t, dir, "add", "tracked")
	gitTest(t, dir, "commit", "-qm", "first")
	gitTest(t, dir, "update-ref", "refs/remotes/origin/main", "HEAD")
	gitTest(t, dir, "checkout", "-qb", "feat/x")
	if err := os.WriteFile(filepath.Join(dir, "tracked"), []byte("after"), 0600); err != nil {
		t.Fatal(err)
	}
	gitTest(t, dir, "add", "tracked")
	gitTest(t, dir, "commit", "-qm", "second")
	gitTest(t, dir, "update-ref", "refs/remotes/origin/feat/x", "HEAD^")
	gitTest(t, dir, "branch", "--set-upstream-to=origin/feat/x", "feat/x")
	if err := os.WriteFile(filepath.Join(dir, "tracked"), []byte("staged"), 0600); err != nil {
		t.Fatal(err)
	}
	gitTest(t, dir, "add", "tracked")
	if err := os.WriteFile(filepath.Join(dir, "tracked"), []byte("modified"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "new"), []byte("new"), 0600); err != nil {
		t.Fatal(err)
	}
	state := collectSnapshot(context.Background(), dir, false)
	var got workspaceState
	if err := json.Unmarshal(state, &got); err != nil {
		t.Fatal(err)
	}
	if got.Kind != "workspace_state" || got.Date != time.Now().Format("2006-01-02") || got.TimeZone == "" || got.Git == nil {
		t.Fatalf("state: %s", state)
	}
	if got.Git.Branch != "feat/x" || got.Git.DefaultBranch != "origin/main" || got.Git.AheadOfDefault == nil || *got.Git.AheadOfDefault != 1 || got.Git.BehindDefault == nil || *got.Git.BehindDefault != 0 || got.Git.Upstream != "origin/feat/x" || got.Git.Ahead == nil || *got.Git.Ahead != 1 || got.Git.Behind == nil || *got.Git.Behind != 0 || got.Git.Staged == nil || *got.Git.Staged != 1 || got.Git.Modified == nil || *got.Git.Modified != 1 || got.Git.Untracked == nil || *got.Git.Untracked != 1 {
		t.Fatalf("state: %s", state)
	}
	// A divergence gives both left and right counts, not just the local tip.
	gitTest(t, dir, "restore", "--staged", "--worktree", "tracked")
	gitTest(t, dir, "checkout", "-q", "main")
	if err := os.WriteFile(filepath.Join(dir, "tracked"), []byte("main"), 0600); err != nil {
		t.Fatal(err)
	}
	gitTest(t, dir, "add", "tracked")
	gitTest(t, dir, "commit", "-qm", "third")
	gitTest(t, dir, "update-ref", "refs/remotes/origin/main", "HEAD")
	gitTest(t, dir, "checkout", "-q", "feat/x")
	state = collectSnapshot(context.Background(), dir, false)
	if err := json.Unmarshal(state, &got); err != nil {
		t.Fatal(err)
	}
	if *got.Git.AheadOfDefault != 1 || *got.Git.BehindDefault != 1 {
		t.Fatalf("diverged: %s", state)
	}
	// A default branch containing the feature's commit yields zero ahead.
	gitTest(t, dir, "update-ref", "refs/remotes/origin/main", "HEAD")
	state = collectSnapshot(context.Background(), dir, false)
	if err := json.Unmarshal(state, &got); err != nil {
		t.Fatal(err)
	}
	if got.Git.AheadOfDefault == nil || *got.Git.AheadOfDefault != 0 {
		t.Fatalf("already on default: %s", state)
	}
	// The symbolic default ref takes priority over the origin/main fallback.
	gitTest(t, dir, "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/feat/x")
	fetch := filepath.Join(dir, ".git", "FETCH_HEAD")
	if err := os.WriteFile(fetch, []byte("local only"), 0600); err != nil {
		t.Fatal(err)
	}
	fetched := time.Date(2026, 9, 26, 14, 2, 11, 0, time.Local)
	if err := os.Chtimes(fetch, fetched, fetched); err != nil {
		t.Fatal(err)
	}
	gitTest(t, dir, "checkout", "--detach", "-q")
	state = collectSnapshot(context.Background(), dir, false)
	if err := json.Unmarshal(state, &got); err != nil {
		t.Fatal(err)
	}
	if got.Git.Branch != "(detached)" || got.Git.DefaultBranch != "origin/feat/x" || got.Git.LastFetch != fetched.Format(time.RFC3339) {
		t.Fatalf("detached and fetched: %s", state)
	}
}

func TestSnapshot_NoRemoteOmitsTheDefaultBranch(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	dir := t.TempDir()
	gitTest(t, dir, "init", "-q", "-b", "main")
	if err := os.WriteFile(filepath.Join(dir, "tracked"), []byte("hello"), 0600); err != nil {
		t.Fatal(err)
	}
	gitTest(t, dir, "add", "tracked")
	gitTest(t, dir, "commit", "-qm", "first")

	state := collectSnapshot(context.Background(), dir, false)
	var got workspaceState
	if err := json.Unmarshal(state, &got); err != nil {
		t.Fatal(err)
	}
	if got.Git == nil || got.Git.Branch != "main" || got.Git.DefaultBranch != "" || got.Git.AheadOfDefault != nil || got.Git.BehindDefault != nil || strings.Contains(string(state), `"default_branch"`) {
		t.Fatalf("no remote should mean no default ref: %s", state)
	}
}

func TestSnapshot_OutsideRepositoryOmitsGit(t *testing.T) {
	state := collectSnapshot(context.Background(), t.TempDir(), false)
	var got workspaceState
	if err := json.Unmarshal(state, &got); err != nil {
		t.Fatal(err)
	}
	if got.Date == "" || got.Git != nil {
		t.Fatalf("state: %s", state)
	}
}
