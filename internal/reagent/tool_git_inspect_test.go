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
	"time"
)

func gitInspectTestGit(t *testing.T, root string, args ...string) string {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	argv := append([]string{"-c", "user.name=Test", "-c", "user.email=test@example.com"}, args...)
	cmd := exec.CommandContext(ctx, "git", argv...)
	cmd.Dir = root
	cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "LANG=C", "LC_ALL=C", "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_SYSTEM=" + os.DevNull, "GIT_CONFIG_GLOBAL=" + os.DevNull}
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v %s", args, err, out)
	}
	return strings.TrimSpace(string(out))
}

func gitInspectTestWrite(t *testing.T, root, name, content string) {
	t.Helper()
	p := filepath.Join(root, name)
	if err := os.MkdirAll(filepath.Dir(p), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(p, []byte(content), 0600); err != nil {
		t.Fatal(err)
	}
}

func gitInspectTestRepo(t *testing.T) *Workspace {
	t.Helper()
	root := t.TempDir()
	gitInspectTestGit(t, root, "init", "-q", "--template="+t.TempDir(), "-b", "main")
	for name, content := range map[string]string{"f.txt": "before\n", "docs/f.txt": "doc before\n", "literal[1].txt": "literal before\n", "literal1.txt": "other before\n", "historical.txt": "old content\n", ".gitattributes": "f.txt diff=evil filter=evil\n", ".env": "SECRET_BASE\n", ".ENV.PROD": "SECRET_CASE\n", "dir/.env.local/a": "SECRET_DIR\n"} {
		gitInspectTestWrite(t, root, name, content)
	}
	gitInspectTestGit(t, root, "add", ".")
	gitInspectTestGit(t, root, "commit", "-q", "-m", "Base")
	gitInspectTestGit(t, root, "checkout", "-q", "-b", "review")
	for name, content := range map[string]string{"f.txt": "after\n", "docs/f.txt": "doc after\n", "literal[1].txt": "literal after\n", "literal1.txt": "other after\n", ".env": "SECRET_NEW\n", ".ENV.PROD": "SECRET_CASE_NEW\n", "dir/.env.local/a": "SECRET_DIR_NEW\n"} {
		gitInspectTestWrite(t, root, name, content)
	}
	if err := os.Remove(filepath.Join(root, "historical.txt")); err != nil {
		t.Fatal(err)
	}
	gitInspectTestGit(t, root, "add", "-A")
	gitInspectTestGit(t, root, "commit", "-q", "-m", "Review branch")
	ws, err := OpenWorkspace(root)
	if err != nil {
		t.Fatal(err)
	}
	return ws
}

func TestGitInspect_OperationsAndLiteralPaths(t *testing.T) {
	ws := gitInspectTestRepo(t)
	for _, tc := range []struct{ args, want, absent string }{
		{`{"operation":"diff","from":"main","to":"review"}`, "+after", "SECRET"},
		{`{"operation":"diff","from":"main","to":"review","stat":true}`, "f.txt", "SECRET"},
		{`{"operation":"diff","from":"main","to":"review","paths":["literal[1].txt"]}`, "literal after", "other after"},
		{`{"operation":"show","rev":"main","path":"historical.txt"}`, "old content", "SECRET"},
		{`{"operation":"show","rev":"review"}`, "Review branch", ".env"},
		{`{"operation":"log","from":"main","to":"review","count":1}`, "Review branch", "Base"},
		{`{"operation":"log","paths":["docs"]}`, "Base", "SECRET"},
	} {
		out := runTool(t, NewGitInspectTool(ws), tc.args)
		var result gitInspectResult
		data(t, out, &result)
		if out.Effect != EffectNone || !strings.Contains(result.Output, tc.want) || strings.Contains(result.Output, tc.absent) {
			t.Fatalf("%s: %+v %+v", tc.args, out, result)
		}
	}
	sub, err := OpenWorkspace(filepath.Join(ws.Root(), "docs"))
	if err != nil {
		t.Fatal(err)
	}
	out := runTool(t, NewGitInspectTool(sub), `{"operation":"show","rev":"main","path":"f.txt"}`)
	var result gitInspectResult
	data(t, out, &result)
	if result.Output != "doc before\n" {
		t.Fatalf("subworkspace leaked root %+v", result)
	}
	out = runTool(t, NewGitInspectTool(sub), `{"operation":"diff","from":"main","to":"HEAD"}`)
	data(t, out, &result)
	if strings.Contains(result.Output, "literal") || !strings.Contains(result.Output, "doc after") {
		t.Fatalf("subworkspace diff %+v", result)
	}
}

func TestGitInspect_RefusesInjectionAndWithheldPaths(t *testing.T) {
	ws := gitInspectTestRepo(t)
	for _, args := range []string{
		`{"operation":"show","rev":"--help"}`, `{"operation":"show","rev":"HEAD:path"}`, `{"operation":"show","rev":"main..HEAD"}`,
		`{"operation":"show","path":"../f.txt"}`, `{"operation":"show","path":".git/config"}`, `{"operation":"show","path":"dir/.EnV.local/a"}`,
		`{"operation":"diff","from":"main","to":"HEAD","paths":["../f.txt"]}`, `{"operation":"diff","from":"main","to":"HEAD","paths":[".GIT"]}`,
		`{"operation":"show","path":"/etc/passwd"}`, `{"operation":"show","path":".env"}`, `{"operation":"log","count":101}`, `{"operation":"log","count":null}`,
		`{"operation":"diff","from":"main"}`, `{"operation":"show","rev":"absent"}`, `{"operation":"log","paths":null}`,
	} {
		out := runTool(t, NewGitInspectTool(ws), args)
		if out.OK || out.Effect != EffectNone {
			t.Fatalf("accepted %s %+v", args, out)
		}
	}
}

func TestGitInspect_NeverRunsRepositoryPrograms(t *testing.T) {
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	marker := filepath.Join(root, "MARKER")
	script := filepath.Join(t.TempDir(), "evil")
	if err := os.WriteFile(script, []byte("#!/bin/sh\ntouch '"+marker+"'\ncat\n"), 0700); err != nil {
		t.Fatal(err)
	}
	// Prove the hostile drivers work before testing the guarded tool.
	for _, driver := range []struct {
		key  string
		args []string
	}{
		{"diff.evil.textconv", []string{"diff", "--no-ext-diff", "--textconv", "main", "HEAD", "--", "f.txt"}},
		{"diff.external", []string{"diff", "--ext-diff", "--no-textconv", "main", "HEAD", "--", "f.txt"}},
		{"filter.evil.clean", []string{"add", "f.txt"}},
	} {
		gitInspectTestGit(t, root, "config", driver.key, script)
		if driver.key == "filter.evil.clean" {
			gitInspectTestWrite(t, root, "f.txt", "positive-control\n")
		}
		gitInspectTestGit(t, root, driver.args...)
		if err := os.Remove(marker); err != nil {
			t.Fatalf("positive control %s did not execute: %v", driver.key, err)
		}
	}
	for _, key := range []string{"filter.evil.clean", "filter.evil.smudge", "filter.evil.process", "diff.evil.textconv", "diff.evil.command", "diff.external", "core.fsmonitor", "core.pager", "pager.diff", "credential.helper", "core.sshCommand", "gpg.program"} {
		gitInspectTestGit(t, root, "config", key, script)
	}
	gitInspectTestGit(t, root, "config", "log.showSignature", "true")
	gitInspectTestGit(t, root, "config", "core.hooksPath", filepath.Dir(script))
	t.Setenv("GIT_EXTERNAL_DIFF", script)
	t.Setenv("GIT_PAGER", script)
	t.Setenv("GIT_CONFIG_COUNT", "1")
	t.Setenv("GIT_CONFIG_KEY_0", "diff.external")
	t.Setenv("GIT_CONFIG_VALUE_0", script)
	before := make(map[string]string)
	for _, name := range []string{"index", "config", "HEAD", "refs/heads/review"} {
		raw, err := os.ReadFile(filepath.Join(root, ".git", name))
		if err != nil {
			t.Fatal(err)
		}
		before[name] = string(raw)
	}
	// Dirty tracked content must never be read by any supported operation.
	gitInspectTestWrite(t, root, "f.txt", "dirty content\n")
	for _, args := range []string{`{"operation":"diff","from":"main","to":"HEAD"}`, `{"operation":"diff","from":"main","to":"HEAD","stat":true}`, `{"operation":"show","rev":"HEAD"}`, `{"operation":"show","path":"f.txt"}`, `{"operation":"log"}`} {
		out := runTool(t, NewGitInspectTool(ws), args)
		if !out.OK {
			t.Fatalf("inspection %+v", out)
		}
	}
	if _, err := os.Lstat(marker); !os.IsNotExist(err) {
		t.Fatalf("repository program ran: %v", err)
	}
	for name, want := range before {
		raw, err := os.ReadFile(filepath.Join(root, ".git", name))
		if err != nil || string(raw) != want {
			t.Fatalf("inspection changed %s", name)
		}
	}
	gitInspectTestGit(t, root, "config", "remote.origin.promisor", "true")
	out := runTool(t, NewGitInspectTool(ws), `{"operation":"show"}`)
	if out.Code != "unsupported_repository" {
		t.Fatalf("partial clone accepted %+v", out)
	}
}

func TestGitInspect_Truncation(t *testing.T) {
	ws := gitInspectTestRepo(t)
	gitInspectTestWrite(t, ws.Root(), "large", strings.Repeat("\x00\xff\n", MaxResultBytes))
	gitInspectTestGit(t, ws.Root(), "add", "large")
	gitInspectTestGit(t, ws.Root(), "commit", "-q", "-m", "Large")
	out := runTool(t, NewGitInspectTool(ws), `{"operation":"show","path":"large"}`)
	var result gitInspectResult
	data(t, out, &result)
	if !out.Truncated || !result.OutputTruncated || !result.EncodingReplaced || result.OutputBytesSeen <= len(result.Output) || encodedSize(out) > MaxResultBytes {
		t.Fatalf("truncation %+v %+v", out, result)
	}
}

func TestGitInspect_ReadAgentReviewAndRootPlan(t *testing.T) {
	ws := gitInspectTestRepo(t)
	for _, agents := range []bool{false, true} {
		registry, err := NewRegistry(Mode{ReadOnly: true}, append([]Tool{NewGitInspectTool(ws)}, NewAgentTools()...)...)
		if err != nil {
			t.Fatal(err)
		}
		cfg := Config{Provider: openaiName, Model: "gpt-6-sol", Registry: registry, Workspace: ws, WorkspacePath: ws.Root(), PlanMode: true, Agents: agents}
		s := NewSession(cfg, NewScriptedModel(), NewTrace(io.Discard), io.Discard)
		t.Cleanup(s.closeAgents)
		inspect := callBlock("inspect", "git_inspect", `{"operation":"diff","from":"main","to":"review"}`)
		if agents {
			s.agentModel = func(Config, *Trace) (Model, error) {
				return NewScriptedModel(turn(inspect), turn(textBlock("reviewed"))), nil
			}
			s.model = NewScriptedModel(turn(spawnBlock("spawn", "review branch", "reviewer"), callBlock("wait", "wait", `{"ids":["reviewer"]}`)), turn(textBlock("done")))
			agentTurn(t, s, "review")
			if len(agentResults(s.history)) != 1 || agentResults(s.history)[0].Result.Reply != "reviewed" {
				t.Fatal("agent review failed")
			}
			child := s.agents.nodes["t1"].session
			if !results(child)[0].Outcome.OK || !strings.Contains(string(results(child)[0].Outcome.Data), "+after") {
				t.Fatal("agent cannot inspect branch")
			}
		} else {
			s.model = NewScriptedModel(turn(inspect), turn(textBlock("reviewed")))
			agentTurn(t, s, "review")
			if !results(s)[0].Outcome.OK || results(s)[0].Outcome.Effect != EffectNone {
				t.Fatal("root plan refused inspection")
			}
		}
	}
}

func TestGitInspect_CLIPlanAgentsOff(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("API_PROXY_URL", "")
	t.Setenv("API_PROXY_PROVIDER", "")
	ws := gitInspectTestRepo(t)
	script := filepath.Join(t.TempDir(), "script.json")
	raw, _ := json.Marshal([]ModelResponse{turn(callBlock("g", "git_inspect", `{"operation":"show","path":"f.txt"}`)), turn(textBlock("reviewed"))})
	if err := os.WriteFile(script, raw, 0600); err != nil {
		t.Fatal(err)
	}
	// CLI integration is covered by the same script format used by the PTY suite.
	var stdout, stderr strings.Builder
	code := Main(context.Background(), []string{"run", "--workspace", ws.Root(), "--plan", "--read-only", "--scripted", script, "review"}, strings.NewReader(""), &stdout, &stderr)
	if code != 0 || !strings.Contains(stdout.String(), "reviewed") {
		t.Fatalf("CLI code%d stdout%s stderr%s", code, stdout.String(), stderr.String())
	}
}

func TestGitInspect_AllTextOperationsTruncateEncodedOutput(t *testing.T) {
	ws := gitInspectTestRepo(t)
	gitInspectTestWrite(t, ws.Root(), "large.txt", strings.Repeat("<\"\\ λ\n", MaxResultBytes))
	gitInspectTestGit(t, ws.Root(), "add", "large.txt")
	gitInspectTestGit(t, ws.Root(), "commit", "-q", "-m", strings.Repeat("long-message ", MaxResultBytes/8))
	for _, args := range []string{`{"operation":"show","path":"large.txt"}`, `{"operation":"show"}`, `{"operation":"diff","from":"main","to":"HEAD"}`, `{"operation":"log","count":1}`} {
		out := runTool(t, NewGitInspectTool(ws), args)
		var result gitInspectResult
		data(t, out, &result)
		if !out.Truncated || !result.OutputTruncated || encodedSize(out) > MaxResultBytes {
			t.Fatalf("unbounded %s %+v", args, out)
		}
	}
}

func TestAgents_ReadStartupRefusesPromisorTraversal(t *testing.T) {
	s := agentFixture(t)
	root := s.workspace.active.Root()
	gitInspectTestGit(t, root, "init", "-q", "-b", "main")
	gitInspectTestWrite(t, root, "a", "a")
	gitInspectTestGit(t, root, "add", "a")
	gitInspectTestGit(t, root, "commit", "-q", "-m", "base")
	marker := filepath.Join(root, "RAN")
	bin := t.TempDir()
	gitInspectTestWrite(t, bin, "git-remote-evil", "#!/bin/sh\ntouch '"+marker+"'\nexit 1\n")
	if err := os.Chmod(filepath.Join(bin, "git-remote-evil"), 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", bin+string(os.PathListSeparator)+os.Getenv("PATH"))
	gitInspectTestGit(t, root, "config", "remote.origin.url", "evil::unused")
	gitInspectTestGit(t, root, "config", "remote.origin.promisor", "true")
	gitInspectTestWrite(t, root, ".git/refs/remotes/origin/main", strings.Repeat("a", 40)+"\n")
	s.agentModel = func(Config, *Trace) (Model, error) {
		return agentTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			var state workspaceState
			json.Unmarshal(req.History[0].User.Workspace, &state)
			if state.Git != nil || state.Counts == "" {
				t.Error("promisor refs traversal was not refused")
			}
			return turn(textBlock("review without unsafe startup")), nil
		}}, nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("spawn", "review", ""), callBlock("wait", "wait", `{"ids":["t1"]}`)), turn(textBlock("done")))
	agentTurn(t, s, "review")
	if _, err := os.Stat(marker); !os.IsNotExist(err) {
		t.Fatal("startup launched remote helper")
	}
}
