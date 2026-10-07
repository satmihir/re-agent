package reagent

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// gitDoRepo is a clone with one commit on main, a bare origin beside it, and
// a stub gh on PATH that keeps one pull request in a file.
func gitDoRepo(t *testing.T) *Workspace {
	t.Helper()
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	base := t.TempDir()
	run := func(dir string, argv ...string) {
		t.Helper()
		command := exec.Command(argv[0], argv[1:]...)
		command.Dir = dir
		if out, err := command.CombinedOutput(); err != nil {
			t.Fatalf("%v: %v\n%s", argv, err, out)
		}
	}
	origin, work := filepath.Join(base, "origin.git"), filepath.Join(base, "work")
	run(base, "git", "init", "-q", "--bare", "-b", "main", origin)
	run(base, "git", "clone", "-q", origin, work)
	run(work, "git", "config", "user.name", "Test")
	run(work, "git", "config", "user.email", "test@example.com")
	if err := os.WriteFile(filepath.Join(work, "README.md"), []byte("hello\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	run(work, "git", "add", "README.md")
	run(work, "git", "commit", "-q", "-m", "First")
	run(work, "git", "push", "-q", "-u", "origin", "main")
	run(work, "git", "remote", "set-head", "origin", "main")

	bin := filepath.Join(base, "bin")
	state := filepath.Join(base, "pr_head")
	stub := `#!/bin/sh
state='` + state + `'
case "$1 $2" in
"pr create")
  while [ $# -gt 0 ]; do [ "$1" = "--head" ] && echo "$2" > "$state"; shift; done
  echo https://example.com/pull/1 ;;
"pr view")
  [ -f "$state" ] || { echo "no pull requests found" >&2; exit 1; }
  head=$(cat "$state")
  case "$*" in *--jq*) echo https://example.com/pull/1 ;;
  *) echo "{\"number\":1,\"url\":\"https://example.com/pull/1\",\"headRefName\":\"$head\",\"comments\":[]}" ;; esac ;;
"pr comment") echo commented ;;
*) echo "unsupported" >&2; exit 1 ;;
esac
`
	if err := os.MkdirAll(bin, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(bin, "gh"), []byte(stub), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", bin+string(os.PathListSeparator)+os.Getenv("PATH"))
	// The child environment keeps HOME; commits need an identity there too.
	home := filepath.Join(base, "home")
	if err := os.MkdirAll(home, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(home, ".gitconfig"), []byte("[user]\n\tname = Test\n\temail = test@example.com\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HOME", home)
	ws, err := OpenWorkspace(work)
	if err != nil {
		t.Fatal(err)
	}
	return ws
}

func gitDoData(t *testing.T, outcome ToolOutcome) gitDoResult {
	t.Helper()
	var result gitDoResult
	if err := json.Unmarshal(outcome.Data, &result); err != nil {
		t.Fatalf("%v: %s", err, outcome.Data)
	}
	return result
}

func TestGitDo_RecipeShipsAPullRequestWithProof(t *testing.T) {
	ws := gitDoRepo(t)
	if err := os.WriteFile(filepath.Join(ws.Root(), "README.md"), []byte("hello\nmore\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	outcome := runTool(t, NewGitDoTool(ws, nil),
		`{"recipe":"ship_pr","branch":"docs/more","message":"Say more","title":"More","body":"Adds a line."}`)
	result := gitDoData(t, outcome)
	if !outcome.OK || result.Status != "done" || outcome.Effect != EffectApplied || result.ChosenBy != "model" {
		t.Fatalf("got %+v / %+v", outcome, result)
	}
	var argv []string
	for _, step := range result.Steps {
		argv = append(argv, strings.Join(step.Argv, " "))
	}
	want := []string{"git switch -q -c docs/more", "git add -A", "git commit -q -m Say more",
		"git push -q -u origin docs/more", "gh pr create --base main --head docs/more --title More --body Adds a line."}
	if strings.Join(argv, "\n") != strings.Join(want, "\n") {
		t.Fatalf("steps:\n%s", strings.Join(argv, "\n"))
	}
	if len(result.Checks) != 3 {
		t.Fatalf("checks %+v", result.Checks)
	}
	for _, c := range result.Checks {
		if !c.OK {
			t.Fatalf("check failed: %+v", c)
		}
	}
}

func TestGitDo_DeclinesWithoutRunningARecipe(t *testing.T) {
	ws := gitDoRepo(t)
	t.Run("infeasible recipe", func(t *testing.T) {
		outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"follow_up_pr"}`)
		result := gitDoData(t, outcome)
		if outcome.OK || outcome.Code != "declined" || outcome.Effect != EffectNone || len(result.Steps) != 0 {
			t.Fatalf("got %+v / %+v", outcome, result)
		}
	})
	t.Run("invalid branch", func(t *testing.T) {
		outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"new_branch","branch":"bad..name"}`)
		if outcome.Code != "declined" || outcome.Effect != EffectNone || !strings.Contains(outcome.Message, "not a valid branch name") {
			t.Fatalf("got %+v", outcome)
		}
	})
	t.Run("Jev failure", func(t *testing.T) {
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			w.WriteHeader(http.StatusServiceUnavailable)
		}))
		defer server.Close()
		outcome := runTool(t, NewGitDoTool(ws, newJevClient("k", server.URL, server.Client())), `{"intent":"show status"}`)
		if outcome.Code != "declined" || outcome.Effect != EffectNone || !strings.Contains(outcome.Message, "Jev could not decide") {
			t.Fatalf("got %+v", outcome)
		}
	})
}

func TestGitDo_FailedStepStopsTheRecipe(t *testing.T) {
	ws := gitDoRepo(t)
	run := exec.Command("git", "remote", "set-url", "origin", filepath.Join(t.TempDir(), "missing.git"))
	run.Dir = ws.Root()
	if out, err := run.CombinedOutput(); err != nil {
		t.Fatalf("%v %s", err, out)
	}
	if err := os.WriteFile(filepath.Join(ws.Root(), "README.md"), []byte("changed\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"ship_pr","branch":"x"}`)
	result := gitDoData(t, outcome)
	last := result.Steps[len(result.Steps)-1]
	if outcome.Code != "step_failed" || outcome.Effect != EffectApplied || last.Argv[1] != "push" || last.ExitCode == 0 {
		t.Fatalf("got %+v / %+v", outcome, result)
	}
}

func TestGitDo_ArgumentsMatchTheMode(t *testing.T) {
	t.Parallel()
	ws := testWorkspace(t, nil)
	for _, c := range []struct {
		tool Tool
		args string
	}{
		{NewGitDoTool(ws, nil), `{"recipe":"nope"}`},
		{NewGitDoTool(ws, nil), `{"recipe":"status","intent":"x"}`},
		{NewGitDoTool(ws, newJevClient("k", "", nil)), `{"recipe":"status"}`},
	} {
		if outcome := runTool(t, c.tool, c.args); outcome.Code != "invalid_arguments" {
			t.Fatalf("%s: got %+v", c.args, outcome)
		}
	}
	for _, tool := range []Tool{NewGitDoTool(ws, nil), NewGitDoTool(ws, newJevClient("k", "", nil))} {
		if _, err := NewRegistry(Mode{}, tool); err != nil {
			t.Fatal(err)
		}
	}
}

// fakeJev answers every question with the given choice and probability, and
// records the criteria it was offered.
func fakeJev(t *testing.T, answers map[string][2]any, offered *map[string]map[string]string) *jevClient {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var request jevRequest
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Fatal(err)
		}
		response := jevResponse{Model: jevModel, Answers: map[string]jevAnswer{}, Usage: &jevUsage{new(int64), new(int64)}}
		if offered != nil {
			*offered = map[string]map[string]string{}
		}
		for name, question := range request.Questions {
			if offered != nil {
				(*offered)[name] = question.Criteria
			}
			choice, p := answers[name][0].(string), answers[name][1].(float64)
			probabilities := map[string]*float64{}
			rest := (1 - p) / float64(len(question.Criteria)-1)
			for id := range question.Criteria {
				value := rest
				if id == choice {
					value = p
				}
				probabilities[id] = &value
			}
			response.Answers[name] = jevAnswer{Type: "choice", Choice: choice, Confidence: &p, Probabilities: probabilities}
		}
		_ = json.NewEncoder(w).Encode(response)
	}))
	t.Cleanup(server.Close)
	return newJevClient("test-key", server.URL, server.Client())
}

func TestGitDo_JevPicksAFeasibleRecipe(t *testing.T) {
	ws := gitDoRepo(t)
	if err := os.WriteFile(filepath.Join(ws.Root(), "README.md"), []byte("changed\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	var offered map[string]map[string]string
	jev := fakeJev(t, map[string][2]any{"outcome": {"commit", 0.95}, "scope": {"git_only", 0.9}}, &offered)
	outcome := runTool(t, NewGitDoTool(ws, jev), `{"intent":"commit my change","message":"Change README"}`)
	result := gitDoData(t, outcome)
	if !outcome.OK || result.Recipe != "commit" || result.ChosenBy != "jev" || result.JevCalls != 1 {
		t.Fatalf("got %+v / %+v", outcome, result)
	}
	// On main with no pull request, the recipes that need one are not offered.
	for _, absent := range []string{"follow_up_pr", "pr_status", "commit_push", "rebase_push"} {
		if _, found := offered["outcome"][absent]; found {
			t.Fatalf("%s offered on main without a pull request: %v", absent, offered["outcome"])
		}
	}
	if offered["outcome"]["none"] == "" || len(offered["scope"]) != 2 {
		t.Fatalf("offered %v", offered)
	}
}

func TestGitDo_JevDeclines(t *testing.T) {
	ws := gitDoRepo(t)
	if err := os.WriteFile(filepath.Join(ws.Root(), "README.md"), []byte("changed\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	for name, answers := range map[string]map[string][2]any{
		"more than git":    {"outcome": {"commit", 0.99}, "scope": {"more", 0.8}},
		"none":             {"outcome": {"none", 0.9}, "scope": {"git_only", 0.9}},
		"low confidence":   {"outcome": {"commit", 0.5}, "scope": {"git_only", 0.9}},
		"change under 0.9": {"outcome": {"commit", 0.85}, "scope": {"git_only", 0.9}},
	} {
		t.Run(name, func(t *testing.T) {
			outcome := runTool(t, NewGitDoTool(ws, fakeJev(t, answers, nil)), `{"intent":"implement it and commit"}`)
			if outcome.Code != "declined" || outcome.Effect != EffectNone {
				t.Fatalf("got %+v", outcome)
			}
		})
	}
}

func TestGitDo_BranchNameFromTheRequest(t *testing.T) {
	for intent, want := range map[string]string{
		"Create and switch to a new branch named feat/status-row starting from the latest main": "feat/status-row",
		"Create a new branch feat/status-row from the latest default branch":                    "feat/status-row",
		"Start a new branch called fix/a.b from main":                                           "fix/a.b",
		"Rebase my branch onto the latest main":                                                 "",
	} {
		run := &gitDoRun{args: gitDoArgs{Intent: intent}}
		if got := run.requestedBranch(); got != want {
			t.Fatalf("%q: got %q, want %q", intent, got, want)
		}
	}
}

func TestJevChoose_AcceptsRoundedProbabilities(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		third, rest := 0.33, 0.0
		_ = json.NewEncoder(w).Encode(jevResponse{Model: jevModel, Usage: &jevUsage{new(int64), new(int64)},
			Answers: map[string]jevAnswer{"q": {Type: "choice", Choice: "a", Confidence: &third,
				Probabilities: map[string]*float64{"a": &third, "b": &third, "c": &third, "d": &rest}}}})
	}))
	defer server.Close()
	jev := newJevClient("k", server.URL, server.Client())
	questions := map[string]jevQuestion{"q": {Type: "choice", Criteria: map[string]string{"a": "A", "b": "B", "c": "C", "d": "D"}}}
	if _, _, err := jev.choose(context.Background(), "state", questions); err != nil {
		t.Fatalf("a sum of 0.99 from rounding was refused: %v", err)
	}
}
