package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
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
# One pull request, kept as its head branch and its state.
state='` + state + `'
case "$1 $2" in
"pr create")
  while [ $# -gt 0 ]; do [ "$1" = "--head" ] && echo "$2" > "$state"; shift; done
  echo OPEN > "$state.state"
  echo https://example.com/pull/1 ;;
"pr view")
  [ -f "$state" ] || { echo "no pull requests found" >&2; exit 1; }
  head=$(cat "$state"); pr_state=$(cat "$state.state" 2>/dev/null || echo OPEN)
  oid=$(git rev-parse "refs/remotes/origin/$head" 2>/dev/null)
  case "$*" in *--jq*) echo https://example.com/pull/1 ;;
  *) echo "{\"number\":1,\"url\":\"https://example.com/pull/1\",\"headRefName\":\"$head\",\"state\":\"$pr_state\",\"headRefOid\":\"$oid\",\"comments\":[]}" ;; esac ;;
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
	if result.Decision.ChosenBy != "model" || result.Decision.Threshold != 0 || result.Decision.Probabilities != nil || result.Decision.Snapshot.Branch != "main" || !reflect.DeepEqual(result.Decision.Snapshot.Changed, []string{"README.md"}) {
		t.Fatalf("decision %+v", result.Decision)
	}
	if len(result.Decision.Offered) == 0 {
		t.Fatalf("missing offered recipes: %+v", result.Decision)
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
		{NewGitDoTool(ws, newJevClient("k", "", nil)), `{"intent":"show status","recipe":"nope"}`},
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
func fakeJev(t *testing.T, answers map[string][2]any, offered *map[string]map[string]string, states ...*string) *jevClient {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var request jevRequest
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Fatal(err)
		}
		if len(states) > 0 {
			*states[0] = request.State
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
	var sent string
	jev := fakeJev(t, map[string][2]any{"outcome": {"commit", 0.95}, "scope": {"git_only", 0.9}}, &offered, &sent)
	outcome := runTool(t, NewGitDoTool(ws, jev), `{"intent":"commit my change","message":"Change README"}`)
	result := gitDoData(t, outcome)
	if !outcome.OK || result.Recipe != "commit" || result.ChosenBy != "jev" || result.JevCalls != 1 {
		t.Fatalf("got %+v / %+v", outcome, result)
	}
	snapshot, err := json.Marshal(result.Decision.Snapshot)
	if err != nil || sent != "Request: commit my change\nRepository: "+string(snapshot) {
		t.Fatalf("Jev received %q; decision snapshot %s: %v", sent, snapshot, err)
	}
	if result.Decision.Threshold != 0.85 || result.Decision.DeclinedBy != "" || result.Decision.ChosenBy != "" || result.Decision.Probabilities["outcome"]["commit"] != 0.95 || result.Decision.Probabilities["scope"]["git_only"] != 0.9 {
		t.Fatalf("decision %+v", result.Decision)
	}
	if len(result.Decision.Offered) != len(offered["outcome"]) {
		t.Fatalf("offered %v / %+v", offered, result.Decision)
	}
	for _, name := range result.Decision.Offered {
		if _, ok := offered["outcome"][name]; !ok {
			t.Fatalf("%s not sent to Jev", name)
		}
	}
	if len(result.Decision.Probabilities["outcome"]) != len(offered["outcome"]) {
		t.Fatalf("missing probabilities: %+v", result.Decision)
	}
	encoded, err := json.Marshal(result.Decision)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("typical git_do decision added bytes: %d", len(encoded)+len(`,"decision":`))
	if len(encoded)+len(`,"decision":`) >= 2048 {
		t.Fatalf("decision exceeds 2 KB: %d", len(encoded))
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

func TestGitDo_JevSnapshotStaysSmallWithManyChanges(t *testing.T) {
	ws := gitDoRepo(t)
	for i := 0; i < 7; i++ {
		cmd := exec.Command("git", "commit", "-q", "--allow-empty", "-m", strings.Repeat("A realistic commit subject ", 6))
		cmd.Dir = ws.Root()
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("commit: %v: %s", err, out)
		}
	}
	for i := 0; i < 120; i++ {
		name := filepath.Join(ws.Root(), fmt.Sprintf("changed-path-with-a-realistic-name-%02d.txt", i))
		if err := os.WriteFile(name, []byte("changed\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	var sent string
	jev := fakeJev(t, map[string][2]any{"outcome": {"status", 1.0}, "scope": {"git_only", 0.9}}, nil, &sent)
	outcome := runTool(t, NewGitDoTool(ws, jev), `{"intent":"show status"}`)
	decision := gitDoData(t, outcome).Decision
	encoded, err := json.Marshal(decision)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("120 paths, 8 long subjects: decision added bytes: %d", len(encoded)+len(`,"decision":`))
	if !outcome.OK || len(encoded)+len(`,"decision":`) >= 2048 || len(decision.Snapshot.Changed) != 12 || decision.Snapshot.ChangedCount != 120 {
		t.Fatalf("result %v; decision %s", outcome.Code, encoded)
	}
	snapshot, err := json.Marshal(decision.Snapshot)
	if err != nil || !strings.HasSuffix(decision.Snapshot.Recent[0], "…") || sent != "Request: show status\nRepository: "+string(snapshot) {
		t.Fatalf("snapshot not sent to Jev: %s: %v", sent, err)
	}
}

func TestGitDo_JevDeclines(t *testing.T) {
	ws := gitDoRepo(t)
	if err := os.WriteFile(filepath.Join(ws.Root(), "README.md"), []byte("changed\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	for name, answers := range map[string]map[string][2]any{
		"more than git":     {"outcome": {"commit", 0.99}, "scope": {"more", 0.8}},
		"none":              {"outcome": {"none", 0.9}, "scope": {"git_only", 0.9}},
		"low confidence":    {"outcome": {"commit", 0.5}, "scope": {"git_only", 0.9}},
		"change under 0.85": {"outcome": {"commit", 0.84}, "scope": {"git_only", 0.9}},
	} {
		t.Run(name, func(t *testing.T) {
			intent := `{"intent":"commit the changed README"}`
			if name == "more than git" {
				intent = `{"intent":"implement it and commit"}`
			}
			outcome := runTool(t, NewGitDoTool(ws, fakeJev(t, answers, nil)), intent)
			if outcome.Code != "declined" || outcome.Effect != EffectNone {
				t.Fatalf("got %+v", outcome)
			}
			decision := gitDoData(t, outcome).Decision
			want := map[string]string{"more than git": "scope", "none": "none", "low confidence": "threshold", "change under 0.85": "threshold"}[name]
			if decision.DeclinedBy != want || decision.Probabilities["outcome"] == nil || decision.Probabilities["scope"] == nil {
				t.Fatalf("decision %+v", decision)
			}
			if want == "threshold" && decision.Threshold != 0.85 {
				t.Fatalf("threshold %+v", decision)
			}
		})
	}
}

func TestGitDo_RevertTargetDecision(t *testing.T) {
	ws := gitDoRepo(t)
	var sent string
	jev := fakeJev(t, map[string][2]any{
		"outcome": {"revert_pr", 0.95}, "scope": {"git_only", 0.9}, "target": {"none", 0.95},
	}, nil, &sent)
	outcome := runTool(t, NewGitDoTool(ws, jev), `{"intent":"revert the unrelated commit"}`)
	decision := gitDoData(t, outcome).Decision
	if outcome.Code != "declined" || decision.DeclinedBy != "target_none" || decision.Threshold != 0.9 || len(decision.Probabilities["target"]) != 2 || !strings.Contains(sent, "Repository: ") {
		t.Fatalf("got %+v / %+v", outcome, decision)
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

// gitDoMergedBranch leaves the clone on feat/done, whose pull request #1 is
// merged into origin's main, with README.md changed again.
func gitDoMergedBranch(t *testing.T, ws *Workspace) {
	t.Helper()
	git := func(argv ...string) {
		t.Helper()
		command := exec.Command("git", argv...)
		command.Dir = ws.Root()
		if out, err := command.CombinedOutput(); err != nil {
			t.Fatalf("%v: %v\n%s", argv, err, out)
		}
	}
	git("switch", "-q", "-c", "feat/done")
	if err := os.WriteFile(filepath.Join(ws.Root(), "README.md"), []byte("hello\ndone\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	git("commit", "-q", "-am", "Done")
	git("push", "-q", "-u", "origin", "feat/done")
	git("push", "-q", "origin", "feat/done:main")
	base := filepath.Dir(ws.Root())
	if err := os.WriteFile(filepath.Join(base, "pr_head"), []byte("feat/done\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(base, "pr_head.state"), []byte("MERGED\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(ws.Root(), "README.md"), []byte("hello\ndone\nnext\n"), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestGitDo_MergedPullRequestIsNotTheBranchs(t *testing.T) {
	ws := gitDoRepo(t)
	gitDoMergedBranch(t, ws)
	run := &gitDoRun{ctx: context.Background(), dir: ws.Root(), result: &gitDoResult{}}
	run.snapshot()
	allowed := run.feasible()
	if run.state.PullRequest != nil || run.state.FinishedPR == nil || run.state.FinishedState != "merged" {
		t.Fatalf("state %+v", run.state)
	}
	if allowed["follow_up_pr"] || !allowed["ship_pr"] || !allowed["pr_status"] {
		t.Fatalf("allowed %v", allowed)
	}
}

func TestGitDo_ShipAfterAMergedPullRequestStartsFromMain(t *testing.T) {
	ws := gitDoRepo(t)
	gitDoMergedBranch(t, ws)
	outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"ship_pr","branch":"feat/next","message":"Next"}`)
	result := gitDoData(t, outcome)
	if !outcome.OK || result.Status != "done" {
		t.Fatalf("got %+v / %+v", outcome, result)
	}
	var argv []string
	for _, step := range result.Steps {
		argv = append(argv, strings.Join(step.Argv, " "))
	}
	if argv[0] != "git fetch -q origin" || argv[1] != "git switch -q -c feat/next --no-track origin/main" {
		t.Fatalf("steps:\n%s", strings.Join(argv, "\n"))
	}
	// The new branch is main plus the one new change, not the finished branch's history.
	count := exec.Command("git", "rev-list", "--count", "origin/main..feat/next")
	count.Dir = ws.Root()
	if out, err := count.Output(); err != nil || strings.TrimSpace(string(out)) != "1" {
		t.Fatalf("feat/next is %q commits past main: %v", out, err)
	}
}

func TestGitDo_ShipDeclinesCommitsAfterAMergedPullRequest(t *testing.T) {
	ws := gitDoRepo(t)
	gitDoMergedBranch(t, ws)
	commit := exec.Command("git", "commit", "-q", "-am", "After the merge")
	commit.Dir = ws.Root()
	if out, err := commit.CombinedOutput(); err != nil {
		t.Fatalf("%v %s", err, out)
	}
	outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"ship_pr","branch":"feat/next"}`)
	if outcome.Code != "declined" || !strings.Contains(outcome.Message, "does not have") || outcome.Effect != EffectNone {
		t.Fatalf("got %+v", outcome)
	}
}

func TestGitDo_ShipCommittedWorkWithoutAMessage(t *testing.T) {
	ws := gitDoRepo(t)
	git := func(argv ...string) {
		t.Helper()
		command := exec.Command("git", argv...)
		command.Dir = ws.Root()
		if out, err := command.CombinedOutput(); err != nil {
			t.Fatalf("%v: %v\n%s", argv, err, out)
		}
	}
	git("switch", "-q", "-c", "feat/ready")
	if err := os.WriteFile(filepath.Join(ws.Root(), "README.md"), []byte("ready\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	git("commit", "-q", "-am", "Ready")
	// Committed work, nothing uncommitted, no message: this used to panic.
	outcome := runTool(t, NewGitDoTool(ws, nil), `{"recipe":"ship_pr"}`)
	if !outcome.OK {
		t.Fatalf("got %+v", outcome)
	}
}

func TestGitDo_JevNamedRecipeThresholdsAndMessages(t *testing.T) {
	state := gitDoState{Branch: "main", Default: "main", Changed: []string{"README.md"}}
	for _, test := range []struct {
		name, intent, named, jevChoice, scope, declinedBy, reason string
		probability, threshold                                    float64
		ran                                                       bool
	}{
		{"explicit agreement", "commit changes", "commit", "commit", "git_only", "", "", 0.72, 0.7, true},
		{"prefix agreement", "commit: record these changes", "", "commit", "git_only", "", "", 0.73, 0.7, true},
		{"disagree", "commit changes", "status", "commit", "git_only", "threshold", "name the matching recipe", 0.88, 0.9, false},
		{"name not allowed", "commit changes", "follow_up_pr", "commit", "git_only", "threshold", "name the matching recipe", 0.88, 0.9, false},
		{"conflicting names", "commit: record changes", "status", "commit", "git_only", "threshold", "name the matching recipe", 0.88, 0.9, false},
		{"unnamed below bar", "commit changes", "", "commit", "git_only", "threshold", "restate the git-only outcome", 0.84, 0.85, false},
		{"unnamed at bar", "commit changes", "", "commit", "git_only", "", "", 0.85, 0.85, true},
		{"scope overrides agreement", "commit: record changes", "", "commit", "more", "scope", "If the work is already done", 0.95, 0, false},
		{"none", "delete branches", "", "none", "git_only", "none", "use exec", 0.9, 0, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			answers := map[string][2]any{"outcome": {test.jevChoice, test.probability}, "scope": {test.scope, 0.9}}
			c := gitDoEvalCase{Arguments: gitDoArgs{Intent: test.intent, Recipe: test.named}, Snapshot: state}
			result, ran, err := gitDoEvalDecision(c, fakeJev(t, answers, nil))
			if err != nil || ran != test.ran || result.Decision.Threshold != test.threshold || result.Decision.DeclinedBy != test.declinedBy || !strings.Contains(result.Reason, test.reason) {
				t.Fatalf("result %+v ran=%t: %v", result, ran, err)
			}
			name := test.named
			if name == "" {
				name = gitDoIntentRecipe(test.intent)
			}
			if name != result.Decision.NamedRecipe || (name != "" && result.Decision.NamedAgreed == nil) {
				t.Fatalf("named decision %+v, want %s", result.Decision, name)
			}
		})
	}
}

func TestGitDo_JevPrefixIsOnlyLeadingRecipe(t *testing.T) {
	for intent, want := range map[string]string{
		"ship_pr: commit and open a PR":          "ship_pr",
		"  follow_up_pr: push and reply":         "follow_up_pr",
		"commit the changes, then ship_pr: a PR": "",
		"not_a_recipe: commit changes":           "",
		"commit_push this branch":                "",
	} {
		if got := gitDoIntentRecipe(intent); got != want {
			t.Fatalf("%q: got %q want %q", intent, got, want)
		}
	}
}

func TestGitDo_JevAcceptsOptionalRecipeArgument(t *testing.T) {
	ws := gitDoRepo(t)
	jev := fakeJev(t, map[string][2]any{"outcome": {"status", 0.76}, "scope": {"git_only", 0.99}}, nil)
	outcome := runTool(t, NewGitDoTool(ws, jev), `{"intent":"show status","recipe":"status"}`)
	result := gitDoData(t, outcome)
	if !outcome.OK || result.Decision.NamedRecipe != "status" || result.Decision.NamedAgreed == nil || !*result.Decision.NamedAgreed || result.Decision.Threshold != 0.7 {
		t.Fatalf("got %+v / %+v", outcome, result)
	}
}

func TestGitDo_JevRejectsExplicitUnfinishedWorkDespiteGitOnlyAnswer(t *testing.T) {
	state := gitDoState{Branch: "main", Default: "main", Changed: []string{"README.md"}}
	for _, intent := range []string{
		"implement the feature, commit and create a PR",
		"Check the PR comments and fix all, commit and respond",
		"check PR comments + fix + push + comment",
	} {
		c := gitDoEvalCase{Arguments: gitDoArgs{Intent: intent}, Snapshot: state}
		answers := map[string][2]any{"outcome": {"commit", 0.99}, "scope": {"git_only", 0.99}}
		result, ran, err := gitDoEvalDecision(c, fakeJev(t, answers, nil))
		if err != nil || ran || result.Decision.DeclinedBy != "scope" || !strings.Contains(result.Reason, "finish it") {
			t.Fatalf("%s: %+v ran=%t: %v", intent, result, ran, err)
		}
	}
}

func TestGitDo_JevRebaseKeepsHighBarWithoutName(t *testing.T) {
	state := gitDoState{Branch: "feat/test", Default: "main"}
	c := gitDoEvalCase{Arguments: gitDoArgs{Intent: "pull from main and try again"}, Snapshot: state}
	answers := map[string][2]any{"outcome": {"rebase_push", 0.88}, "scope": {"git_only", 0.99}}
	result, ran, err := gitDoEvalDecision(c, fakeJev(t, answers, nil))
	if err != nil || ran || result.Decision.Threshold != 0 || result.Decision.DeclinedBy != "scope" {
		t.Fatalf("%+v ran=%t: %v", result, ran, err)
	}
	c.Arguments.Intent = "rebase branch onto main and update remote"
	result, ran, err = gitDoEvalDecision(c, fakeJev(t, answers, nil))
	if err != nil || ran || result.Decision.Threshold != 0.9 || result.Decision.DeclinedBy != "threshold" {
		t.Fatalf("%+v ran=%t: %v", result, ran, err)
	}
}
