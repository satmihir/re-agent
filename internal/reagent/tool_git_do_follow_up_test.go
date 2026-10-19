package reagent

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func gitDoOpenPR(t *testing.T, ws *Workspace) {
	t.Helper()
	gitDoGit(t, ws.Root(), "switch", "-q", "-c", "feature")
	gitDoWrite(t, ws.Root(), "intended.txt")
	gitDoGit(t, ws.Root(), "add", "intended.txt")
	gitDoGit(t, ws.Root(), "commit", "-q", "-m", "Intended")
	gitDoGit(t, ws.Root(), "push", "-q", "-u", "origin", "feature")
	if err := os.WriteFile(filepath.Join(filepath.Dir(ws.Root()), "pr_head"), []byte("feature\n"), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestGitDo_FollowUpPushesRewrittenBranchWithObservedLease(t *testing.T) {
	for _, history := range []string{"fast-forward", "amend", "rebase"} {
		t.Run(history, func(t *testing.T) {
			ws := gitDoRepo(t)
			gitDoOpenPR(t, ws)
			observed := gitDoGit(t, ws.Root(), "rev-parse", "HEAD")
			switch history {
			case "amend":
				gitDoGit(t, ws.Root(), "commit", "-q", "--amend", "-m", "Amended")
			case "rebase":
				gitDoGit(t, ws.Root(), "switch", "-q", "main")
				gitDoWrite(t, ws.Root(), "base.txt")
				gitDoGit(t, ws.Root(), "add", "base.txt")
				gitDoGit(t, ws.Root(), "commit", "-q", "-m", "Base")
				gitDoGit(t, ws.Root(), "switch", "-q", "feature")
				gitDoGit(t, ws.Root(), "rebase", "main")
			}
			gitDoWrite(t, ws.Root(), "review.txt")
			outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"follow_up_pr"}`)
			result := gitDoData(t, outcome)
			if !outcome.OK {
				t.Fatalf("%s: %s", result.Status, result.Reason)
			}
			want := "git push -q -u origin feature"
			if history != "fast-forward" {
				want = "git push -q -u --force-with-lease=feature:" + observed + " origin feature"
			}
			push, comment := false, false
			for _, step := range result.Steps {
				if strings.Join(step.Argv, " ") == want {
					push = true
				}
				if strings.HasPrefix(strings.Join(step.Argv, " "), "gh pr comment 1 --body Addressed in ") {
					comment = true
				}
			}
			if !push || !comment {
				t.Fatalf("missing push or comment: %+v", result.Steps)
			}
			if history != "fast-forward" {
				lease := false
				for _, check := range result.Checks {
					if check.OK && check.Check == "rewrote branch; lease "+observed {
						lease = true
					}
				}
				if !lease {
					t.Fatalf("missing lease proof: %+v", result.Checks)
				}
			}
			if remote := gitDoGit(t, ws.Root(), "ls-remote", "origin", "refs/heads/feature"); !strings.HasPrefix(remote, gitDoGit(t, ws.Root(), "rev-parse", "HEAD")+"\t") {
				t.Fatalf("remote did not reach HEAD: %s", remote)
			}
		})
	}
}

func TestGitDo_FollowUpDeclinesMovedRemoteBeforeCommit(t *testing.T) {
	ws := gitDoRepo(t)
	gitDoOpenPR(t, ws)
	gitDoGit(t, ws.Root(), "commit", "-q", "--amend", "-m", "Amended")
	before := gitDoGit(t, ws.Root(), "rev-parse", "HEAD")
	gitDoWrite(t, ws.Root(), "review.txt")
	realGit, err := exec.LookPath("git")
	if err != nil {
		t.Fatal(err)
	}
	bin := t.TempDir()
	marker := filepath.Join(bin, "observed")
	origin := filepath.Join(filepath.Dir(ws.Root()), "origin.git")
	// Move the real bare remote just after its first preflight observation.
	wrapper := fmt.Sprintf(`#!/bin/sh
if [ "$1" = "ls-remote" ] && [ ! -f '%s' ]; then
  '%s' "$@" || exit
  touch '%s'
  '%s' --git-dir='%s' update-ref refs/heads/feature refs/heads/main
  exit
fi
exec '%s' "$@"
`, marker, realGit, marker, realGit, origin, realGit)
	if err := os.WriteFile(filepath.Join(bin, "git"), []byte(wrapper), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", bin+string(os.PathListSeparator)+os.Getenv("PATH"))
	outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"follow_up_pr"}`)
	if outcome.Code != "declined" || outcome.Message != "the remote branch moved; fetch and reconcile first" || len(gitDoData(t, outcome).Steps) != 0 {
		t.Fatalf("%s: %s; steps=%d", outcome.Code, outcome.Message, len(gitDoData(t, outcome).Steps))
	}
	if gitDoGit(t, ws.Root(), "rev-parse", "HEAD") != before || gitDoGit(t, ws.Root(), "diff", "--cached", "--name-only") != "" {
		t.Fatal("committed or staged despite moved remote")
	}
}

func TestGitDo_FollowUpPreservesFetchedCollaboratorCommit(t *testing.T) {
	ws := gitDoRepo(t)
	gitDoOpenPR(t, ws)
	before := gitDoGit(t, ws.Root(), "rev-parse", "HEAD")
	base := filepath.Dir(ws.Root())
	collaborator := filepath.Join(base, "collaborator")
	gitDoGit(t, base, "clone", "-q", "--template=", "-b", "feature", filepath.Join(base, "origin.git"), collaborator)
	gitDoWrite(t, collaborator, "collaborator.txt")
	gitDoGit(t, collaborator, "add", "collaborator.txt")
	gitDoGit(t, collaborator, "commit", "-q", "-m", "Collaborator review")
	gitDoGit(t, collaborator, "push", "-q", "origin", "feature")
	remote := gitDoGit(t, collaborator, "rev-parse", "HEAD")
	gitDoGit(t, ws.Root(), "fetch", "-q", "origin")
	gitDoGit(t, ws.Root(), "cat-file", "-e", remote+"^{commit}")
	gitDoWrite(t, ws.Root(), "review.txt")
	outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"follow_up_pr"}`)
	if outcome.Code != "declined" || outcome.Message != "the remote branch has commits that HEAD does not contain; merge or rebase onto it first" || len(gitDoData(t, outcome).Steps) != 0 {
		t.Errorf("%s: %s; steps=%+v", outcome.Code, outcome.Message, gitDoData(t, outcome).Steps)
	}
	if gitDoGit(t, ws.Root(), "rev-parse", "HEAD") != before || gitDoGit(t, ws.Root(), "diff", "--cached", "--name-only") != "" || gitDoGit(t, ws.Root(), "status", "--porcelain") != "?? review.txt" {
		t.Error("changed local HEAD, index or review edit")
	}
	if tip := gitDoGit(t, ws.Root(), "ls-remote", "origin", "refs/heads/feature"); tip != remote+"\trefs/heads/feature" {
		t.Errorf("overwrote collaborator commit %s: remote=%s", remote, tip)
	}
}

func TestGitDo_FollowUpDeclinesWithoutOwnBranchReflogProof(t *testing.T) {
	for _, proof := range []string{"other-branch-only", "unavailable"} {
		t.Run(proof, func(t *testing.T) {
			ws := gitDoRepo(t)
			gitDoOpenPR(t, ws)
			remote := gitDoGit(t, ws.Root(), "rev-parse", "HEAD")
			gitDoGit(t, ws.Root(), "branch", "preserved")
			gitDoGit(t, ws.Root(), "commit", "-q", "--amend", "-m", "Amended")
			before := gitDoGit(t, ws.Root(), "rev-parse", "HEAD")
			gitDoWrite(t, ws.Root(), "review.txt")
			if proof == "other-branch-only" {
				gitDoGit(t, ws.Root(), "reflog", "expire", "--expire=all", "refs/heads/feature")
				if gitDoGit(t, ws.Root(), "reflog", "show", "--format=%H", "refs/heads/preserved") != remote {
					t.Fatal("missing other branch's reflog proof")
				}
			} else {
				realGit, err := exec.LookPath("git")
				if err != nil {
					t.Fatal(err)
				}
				bin := t.TempDir()
				wrapper := fmt.Sprintf("#!/bin/sh\n[ \"$1\" = reflog ] && exit 128\nexec '%s' \"$@\"\n", realGit)
				if err := os.WriteFile(filepath.Join(bin, "git"), []byte(wrapper), 0o755); err != nil {
					t.Fatal(err)
				}
				t.Setenv("PATH", bin+string(os.PathListSeparator)+os.Getenv("PATH"))
			}
			outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"follow_up_pr"}`)
			if outcome.Code != "declined" || outcome.Message != "the remote branch has commits that HEAD does not contain; merge or rebase onto it first" || len(gitDoData(t, outcome).Steps) != 0 {
				t.Errorf("%s: %s; steps=%+v", outcome.Code, outcome.Message, gitDoData(t, outcome).Steps)
			}
			if gitDoGit(t, ws.Root(), "rev-parse", "HEAD") != before || gitDoGit(t, ws.Root(), "diff", "--cached", "--name-only") != "" || gitDoGit(t, ws.Root(), "status", "--porcelain") != "?? review.txt" {
				t.Error("changed local HEAD, index or review edit")
			}
			if tip := gitDoGit(t, ws.Root(), "ls-remote", "origin", "refs/heads/feature"); tip != remote+"\trefs/heads/feature" {
				t.Errorf("rewrote branch without proof: remote=%s", tip)
			}
		})
	}
}
