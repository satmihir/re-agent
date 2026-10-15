package reagent

import (
	"context"
	"crypto/sha1"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func gitInspectWorktreeResult(t *testing.T, ws *Workspace, args string) (ToolOutcome, gitInspectResult) {
	t.Helper()
	out := runTool(t, NewGitInspectTool(ws), args)
	var result gitInspectResult
	data(t, out, &result)
	if out.Effect != EffectNone || encodedSize(out) > MaxResultBytes {
		t.Fatalf("inspection effect or size: %+v", out)
	}
	return out, result
}

func gitInspectWorktreeContains(t *testing.T, output string, fragments ...string) {
	t.Helper()
	for _, fragment := range fragments {
		if !strings.Contains(output, fragment) {
			t.Fatalf("missing %q in:\n%s", fragment, output)
		}
	}
}

func gitInspectWorktreeLine(t *testing.T, output string, fragments ...string) {
	t.Helper()
	for _, line := range strings.Split(output, "\n") {
		found := true
		for _, fragment := range fragments {
			if fragment == "staged" || fragment == "unstaged" {
				fields := strings.Fields(line)
				found = found && len(fields) > 0 && strings.TrimSuffix(fields[0], ":") == fragment
				continue
			}
			found = found && strings.Contains(line, fragment)
		}
		if found {
			return
		}
	}
	t.Fatalf("no line containing %q in:\n%s", fragments, output)
}

func TestGitInspect_WorktreeStatusAndDiffLayers(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	gitInspectTestWrite(t, root, "f.txt", "staged text\n")
	gitInspectTestWrite(t, root, "added.txt", "added text\n")
	gitInspectTestGit(t, root, "add", "f.txt", "added.txt")
	gitInspectTestGit(t, root, "rm", "docs/f.txt")
	gitInspectTestWrite(t, root, "f.txt", "working text\n")
	gitInspectTestWrite(t, root, "literal[1].txt", "literal working\n")
	gitInspectTestWrite(t, root, "untracked.txt", "untracked content\n")
	if err := os.Remove(filepath.Join(root, "literal1.txt")); err != nil {
		t.Fatal(err)
	}
	_, status := gitInspectWorktreeResult(t, ws, `{"operation":"status"}`)
	for _, fragments := range [][]string{
		{"staged", "modified", `"f.txt"`},
		{"unstaged", "modified", `"f.txt"`},
		{"staged", "added", `"added.txt"`},
		{"staged", "deleted", `"docs/f.txt"`},
		{"unstaged", "modified", `"literal[1].txt"`},
		{"unstaged", "deleted", `"literal1.txt"`},
		{"untracked", `"untracked.txt"`},
	} {
		gitInspectWorktreeLine(t, status.Output, fragments...)
	}
	if status.Rev != gitInspectTestGit(t, root, "rev-parse", "HEAD") {
		t.Fatalf("status default revision: %+v", status)
	}
	_, otherStatus := gitInspectWorktreeResult(t, ws, `{"operation":"status","rev":"main","paths":["f.txt"]}`)
	if otherStatus.Rev != gitInspectTestGit(t, root, "rev-parse", "main") {
		t.Fatalf("status explicit revision: %+v", otherStatus)
	}
	gitInspectWorktreeLine(t, otherStatus.Output, "staged", "modified", `"f.txt"`)
	for _, tc := range []struct {
		args string
		want []string
		omit []string
	}{
		{`{"operation":"diff","staged":true,"paths":["f.txt"]}`, []string{"@@", "-after", "+staged text"}, []string{"working text", "untracked content"}},
		{`{"operation":"diff","staged":true,"from":"main","paths":["f.txt"]}`, []string{"-before", "+staged text"}, []string{"working text"}},
		{`{"operation":"worktree_diff","paths":["f.txt"]}`, []string{"@@", "-after", "+working text"}, []string{"staged text", "untracked content"}},
		{`{"operation":"worktree_diff","rev":"main","paths":["f.txt"]}`, []string{"-before", "+working text"}, []string{"staged text"}},
		{`{"operation":"worktree_diff","paths":["literal[1].txt"]}`, []string{"literal working"}, []string{"literal1.txt", "working text"}},
		{`{"operation":"worktree_diff","paths":["untracked.txt"]}`, []string{"+untracked content"}, []string{"working text"}},
		{`{"operation":"diff","staged":true,"stat":true}`, []string{"f.txt", "added.txt", "docs/f.txt"}, []string{"@@", "+staged text"}},
		{`{"operation":"worktree_diff","stat":true}`, []string{"f.txt", "added.txt", "literal1.txt"}, []string{"@@", "+working text", "untracked content"}},
	} {
		t.Run(tc.args, func(t *testing.T) {
			_, result := gitInspectWorktreeResult(t, ws, tc.args)
			gitInspectWorktreeContains(t, result.Output, tc.want...)
			for _, fragment := range tc.omit {
				if strings.Contains(result.Output, fragment) {
					t.Fatalf("unexpected %q in %s", fragment, result.Output)
				}
			}
		})
	}
}

func TestGitInspect_WorktreeTypesAndMode(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	gitInspectTestWrite(t, root, "executable.txt", "unchanged\n")
	gitInspectTestWrite(t, root, "regular.txt", "regular\n")
	if err := os.Symlink("first-target", filepath.Join(root, "link")); err != nil {
		t.Fatal(err)
	}
	base := gitInspectTestGit(t, root, "rev-parse", "main")
	gitInspectTestGit(t, root, "add", "executable.txt", "regular.txt", "link")
	gitInspectTestGit(t, root, "update-index", "--add", "--cacheinfo", "160000,"+base+",module")
	gitInspectTestGit(t, root, "commit", "-q", "-m", "Types")
	outside := t.TempDir()
	gitInspectTestWrite(t, outside, "secret", "DO_NOT_FOLLOW_SYMLINK\n")
	for _, name := range []string{"regular.txt", "link"} {
		if err := os.Remove(filepath.Join(root, name)); err != nil {
			t.Fatal(err)
		}
		if err := os.Symlink(filepath.Join(outside, "secret"), filepath.Join(root, name)); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.Chmod(filepath.Join(root, "executable.txt"), 0700); err != nil {
		t.Fatal(err)
	}
	gitInspectTestWrite(t, root, "module/secret", "DO_NOT_DESCEND_SUBMODULE\n")
	head := gitInspectTestGit(t, root, "rev-parse", "HEAD")
	gitInspectTestGit(t, root, "update-index", "--cacheinfo", "160000,"+head+",module")
	_, status := gitInspectWorktreeResult(t, ws, `{"operation":"status"}`)
	gitInspectWorktreeLine(t, status.Output, "unstaged", "mode", `"executable.txt"`)
	gitInspectWorktreeLine(t, status.Output, "unstaged", "type", `"regular.txt"`)
	gitInspectWorktreeLine(t, status.Output, "submodule", `"module"`)
	_, mode := gitInspectWorktreeResult(t, ws, `{"operation":"worktree_diff","paths":["executable.txt"]}`)
	gitInspectWorktreeContains(t, mode.Output, "mode")
	if strings.Contains(mode.Output, "@@") {
		t.Fatalf("mode-only change emitted a content hunk: %s", mode.Output)
	}
	for _, args := range []string{`{"operation":"worktree_diff"}`, `{"operation":"diff","staged":true}`} {
		_, result := gitInspectWorktreeResult(t, ws, args)
		if strings.Contains(result.Output, "DO_NOT_") || strings.Contains(result.Output, "module/secret") {
			t.Fatalf("followed link or submodule: %s", result.Output)
		}
		if strings.Contains(args, "worktree_diff") {
			gitInspectWorktreeContains(t, result.Output, "mode", "link", "first-target", filepath.Join(outside, "secret"), "submodule")
		} else {
			gitInspectWorktreeContains(t, result.Output, "Subproject commit", "module")
		}
	}
	gitInspectTestGit(t, root, "add", "executable.txt", "regular.txt", "link")
	_, status = gitInspectWorktreeResult(t, ws, `{"operation":"status"}`)
	gitInspectWorktreeLine(t, status.Output, "staged", "mode", `"executable.txt"`)
	gitInspectWorktreeLine(t, status.Output, "staged", "type", `"regular.txt"`)
}

func TestGitInspect_WorktreeBinaryAndOversizeAreSummaries(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	before := "old\x00binary\n"
	after := "new\x00binary\n"
	largeBefore := strings.Repeat("a", (1<<20)+1)
	largeAfter := strings.Repeat("b", (1<<20)+2)
	gitInspectTestWrite(t, root, "binary", before)
	gitInspectTestWrite(t, root, "large", largeBefore)
	gitInspectTestGit(t, root, "add", "binary", "large")
	gitInspectTestGit(t, root, "commit", "-q", "-m", "Summary baseline")
	gitInspectTestWrite(t, root, "binary", after)
	gitInspectTestWrite(t, root, "large", largeAfter)
	for _, args := range []string{`{"operation":"worktree_diff"}`, `{"operation":"worktree_diff","stat":true}`, `{"operation":"status"}`} {
		out, result := gitInspectWorktreeResult(t, ws, args)
		gitInspectWorktreeContains(t, result.Output, "binary", "large")
		for _, content := range []string{before, after, largeBefore, largeAfter} {
			gitInspectWorktreeContains(t, result.Output, fmt.Sprint(len(content)), fmt.Sprintf("%x", sha1.Sum([]byte(fmt.Sprintf("blob %d\x00%s", len(content), content)))))
		}
		if out.Truncated || strings.Contains(result.Output, "\x00") || strings.Contains(result.Output, strings.Repeat("a", 100)) || strings.Contains(result.Output, strings.Repeat("b", 100)) {
			t.Fatalf("content was emitted instead of summary: %+v %s", out, result.Output)
		}
	}
}

func TestGitInspect_WorktreeComparisonIsNotFilterAware(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	gitInspectTestWrite(t, root, ".gitattributes", "f.txt text eol=lf\n")
	gitInspectTestGit(t, root, "add", ".gitattributes")
	gitInspectTestGit(t, root, "commit", "-q", "-m", "Text attribute")
	gitInspectTestGit(t, root, "config", "core.autocrlf", "true")
	gitInspectTestWrite(t, root, "f.txt", "after\r\n")
	for _, args := range []string{`{"operation":"status"}`, `{"operation":"worktree_diff","paths":["f.txt"]}`} {
		_, result := gitInspectWorktreeResult(t, ws, args)
		gitInspectWorktreeContains(t, result.Output, "comparison not filter-aware", "f.txt")
	}
}

func TestGitInspect_WorktreeWithheldSubworkspaceAndLiteralPaths(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	for name, content := range map[string]string{
		"f.txt": "ROOT_ONLY\n", "docs/f.txt": "DOC_ONLY\n", "docs/literal[1]": "LITERAL_ONLY\n", "docs/literal1": "OTHER_LITERAL\n",
		".env": "SECRET_TRACKED\n", "dir/.env.local/a": "SECRET_NESTED\n", "docs/.ENV.EXTRA": "SECRET_UNTRACKED\n",
	} {
		gitInspectTestWrite(t, root, name, content)
	}
	gitInspectTestGit(t, root, "add", "f.txt", "docs/f.txt", ".env", "dir/.env.local/a")
	sub, err := OpenWorkspace(filepath.Join(root, "docs"))
	if err != nil {
		t.Fatal(err)
	}
	for _, args := range []string{`{"operation":"status"}`, `{"operation":"worktree_diff"}`, `{"operation":"diff","staged":true}`} {
		_, result := gitInspectWorktreeResult(t, sub, args)
		gitInspectWorktreeContains(t, result.Output, "f.txt")
		for _, forbidden := range []string{"ROOT_ONLY", "SECRET", ".env", ".ENV", "docs/f.txt", "dir/"} {
			if strings.Contains(result.Output, forbidden) {
				t.Fatalf("subworkspace/withheld leakage %q in %s", forbidden, result.Output)
			}
		}
		_, result = gitInspectWorktreeResult(t, ws, args)
		if strings.Contains(result.Output, "SECRET") || strings.Contains(result.Output, ".env") || strings.Contains(result.Output, ".ENV") {
			t.Fatalf("withheld root leakage: %s", result.Output)
		}
	}
	_, result := gitInspectWorktreeResult(t, sub, `{"operation":"status","paths":["literal[1]"]}`)
	gitInspectWorktreeContains(t, result.Output, `"literal[1]"`)
	if strings.Contains(result.Output, `"literal1"`) {
		t.Fatalf("glob expansion: %s", result.Output)
	}
}

func TestGitInspect_WorktreeUntrackedInventoryIsBounded(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	for i := 0; i < 1001; i++ {
		gitInspectTestWrite(t, ws.Root(), fmt.Sprintf("untracked-%04d", i), "not emitted\n")
	}
	out, result := gitInspectWorktreeResult(t, ws, `{"operation":"status"}`)
	if !out.Truncated || !result.OutputTruncated || strings.Count(result.Output, `"untracked-`) > 1000 || strings.Contains(result.Output, `"untracked-1000"`) {
		t.Fatalf("untracked bound not reported honestly: %+v %+v", out, result)
	}
	gitInspectWorktreeContains(t, result.Output, `"untracked-0000"`)
}

func TestGitInspect_WorktreeIndexFlagsAreNotSilentlyClean(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	gitInspectTestGit(t, root, "update-index", "--assume-unchanged", "f.txt")
	gitInspectTestGit(t, root, "update-index", "--skip-worktree", "docs/f.txt")
	gitInspectTestWrite(t, root, "f.txt", "assumed dirty\n")
	if err := os.Remove(filepath.Join(root, "docs/f.txt")); err != nil {
		t.Fatal(err)
	}
	gitInspectTestWrite(t, root, "pending.txt", "intent content\n")
	gitInspectTestGit(t, root, "add", "-N", "pending.txt")
	_, status := gitInspectWorktreeResult(t, ws, `{"operation":"status"}`)
	gitInspectWorktreeLine(t, status.Output, "assume", `"f.txt"`)
	gitInspectWorktreeLine(t, status.Output, "skip", `"docs/f.txt"`)
	gitInspectWorktreeLine(t, status.Output, "intent", `"pending.txt"`)
	_, result := gitInspectWorktreeResult(t, ws, `{"operation":"worktree_diff"}`)
	gitInspectWorktreeContains(t, result.Output, "assume-unchanged", "skip-worktree", "intent-to-add")
	if strings.Contains(result.Output, "-doc after") {
		t.Fatalf("sparse absence treated as deletion: %s", result.Output)
	}
}

func TestGitInspect_WorktreeSHA256Repository(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "git", "init", "-q", "--object-format=sha256", "--template="+t.TempDir(), "-b", "main")
	cmd.Dir = root
	cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL=" + os.DevNull, "GIT_CONFIG_SYSTEM=" + os.DevNull}
	if output, err := cmd.CombinedOutput(); err != nil {
		t.Skipf("Git cannot initialize SHA-256 repositories: %v %s", err, output)
	}
	gitInspectTestWrite(t, root, "file", "before\n")
	gitInspectTestGit(t, root, "add", "file")
	gitInspectTestGit(t, root, "commit", "-q", "-m", "Base")
	ws, err := OpenWorkspace(root)
	if err != nil {
		t.Fatal(err)
	}
	gitInspectTestWrite(t, root, "file", "staged\n")
	gitInspectTestGit(t, root, "add", "file")
	gitInspectTestWrite(t, root, "file", "working\n")
	_, result := gitInspectWorktreeResult(t, ws, `{"operation":"status"}`)
	if len(result.Rev) != 64 {
		t.Fatalf("not a SHA-256 revision: %+v", result)
	}
	for _, tc := range []struct{ args, want string }{
		{`{"operation":"diff","staged":true}`, "+staged"},
		{`{"operation":"worktree_diff"}`, "+working"},
	} {
		_, result := gitInspectWorktreeResult(t, ws, tc.args)
		gitInspectWorktreeContains(t, result.Output, "-before", tc.want)
	}
}

func TestGitInspect_WorktreeRejectsInvalidArguments(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	for _, args := range []string{
		`{"operation":"status","stat":true}`, `{"operation":"status","from":"HEAD"}`, `{"operation":"status","to":"HEAD"}`,
		`{"operation":"status","staged":true}`, `{"operation":"status","path":"f.txt"}`, `{"operation":"status","count":1}`,
		`{"operation":"worktree_diff","from":"HEAD"}`, `{"operation":"worktree_diff","to":"HEAD"}`, `{"operation":"worktree_diff","staged":true}`,
		`{"operation":"worktree_diff","count":1}`, `{"operation":"worktree_diff","path":"f.txt"}`, `{"operation":"worktree_diff","stat":"yes"}`,
		`{"operation":"diff","staged":true,"to":"HEAD"}`, `{"operation":"diff","staged":true,"rev":"HEAD"}`, `{"operation":"diff","staged":"true"}`,
		`{"operation":"diff","staged":null}`, `{"operation":"status","rev":null}`, `{"operation":"status","paths":null}`,
		`{"operation":"status","rev":"--help"}`, `{"operation":"worktree_diff","rev":"HEAD:file"}`, `{"operation":"worktree_diff","rev":"main..HEAD"}`,
		`{"operation":"status","paths":["../f.txt"]}`, `{"operation":"worktree_diff","paths":[".git/config"]}`, `{"operation":"diff","staged":true,"paths":["dir/.EnV.local/a"]}`,
		`{"operation":"status","paths":["/etc/passwd"]}`, `{"operation":"worktree_diff","rev":"absent"}`,
		`{"operation":"status","paths":[` + strings.TrimSuffix(strings.Repeat(`"f.txt",`, 33), ",") + `]}`,
		`{"operation":"show","staged":true}`, `{"operation":"log","staged":true}`,
	} {
		t.Run(args, func(t *testing.T) {
			out := runTool(t, NewGitInspectTool(ws), args)
			if out.OK || out.Effect != EffectNone {
				t.Fatalf("accepted invalid arguments: %+v", out)
			}
		})
	}
}

func TestGitInspect_WorktreeNeverRunsRepositoryPrograms(t *testing.T) {
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	marker := filepath.Join(t.TempDir(), "MARKER")
	script := filepath.Join(t.TempDir(), "driver")
	if err := os.WriteFile(script, []byte("#!/bin/sh\ntouch '"+marker+"'\ncat\n"), 0700); err != nil {
		t.Fatal(err)
	}
	for _, driver := range []struct {
		key  string
		args []string
	}{
		{"filter.evil.clean", []string{"add", "f.txt"}},
		{"filter.evil.smudge", []string{"checkout", "--", "f.txt"}},
		{"diff.evil.textconv", []string{"diff", "--no-ext-diff", "--textconv", "main", "HEAD", "--", "f.txt"}},
		{"diff.external", []string{"diff", "--ext-diff", "--no-textconv", "main", "HEAD", "--", "f.txt"}},
		{"core.fsmonitor", []string{"status", "--porcelain", "-uno"}},
	} {
		gitInspectTestGit(t, root, "config", driver.key, script)
		gitInspectTestWrite(t, root, "f.txt", "positive control "+driver.key+"\n")
		gitInspectTestGit(t, root, driver.args...)
		if err := os.Remove(marker); err != nil {
			t.Fatalf("positive control %s did not execute: %v", driver.key, err)
		}
		gitInspectTestGit(t, root, "config", "--unset", driver.key)
	}
	gitInspectTestWrite(t, root, "f.txt", "staged change\n")
	gitInspectTestGit(t, root, "add", "f.txt")
	gitInspectTestWrite(t, root, "f.txt", "working change\n")
	for _, key := range []string{"filter.evil.clean", "filter.evil.smudge", "filter.evil.process", "diff.evil.textconv", "diff.evil.command", "diff.external", "core.fsmonitor"} {
		gitInspectTestGit(t, root, "config", key, script)
	}
	t.Setenv("GIT_EXTERNAL_DIFF", script)
	t.Setenv("GIT_CONFIG_COUNT", "1")
	t.Setenv("GIT_CONFIG_KEY_0", "core.fsmonitor")
	t.Setenv("GIT_CONFIG_VALUE_0", script)
	before := make(map[string]string)
	for _, name := range []string{"index", "config", "HEAD", "refs/heads/review"} {
		raw, err := os.ReadFile(filepath.Join(root, ".git", name))
		if err != nil {
			t.Fatal(err)
		}
		before[name] = string(raw)
	}
	for _, args := range []string{
		`{"operation":"status"}`, `{"operation":"worktree_diff"}`, `{"operation":"worktree_diff","stat":true}`,
		`{"operation":"diff","staged":true}`, `{"operation":"diff","staged":true,"stat":true}`,
	} {
		gitInspectWorktreeResult(t, ws, args)
		if _, err := os.Lstat(marker); !os.IsNotExist(err) {
			t.Fatalf("repository program ran for %s: %v", args, err)
		}
	}
	for name, want := range before {
		raw, err := os.ReadFile(filepath.Join(root, ".git", name))
		if err != nil || string(raw) != want {
			t.Fatalf("inspection changed %s: %v", name, err)
		}
	}
}

func TestGitInspect_WorktreePatchOutputIsBounded(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	gitInspectTestWrite(t, ws.Root(), "f.txt", strings.Repeat(strings.Repeat("patch line ", 100)+"\n", 200)+"END_OF_PATCH\n")
	for _, args := range []string{`{"operation":"worktree_diff"}`, `{"operation":"diff","staged":true}`} {
		if strings.Contains(args, "staged") {
			gitInspectTestGit(t, ws.Root(), "add", "f.txt")
		}
		out, result := gitInspectWorktreeResult(t, ws, args)
		if !out.Truncated || !result.OutputTruncated || result.OutputBytesSeen <= len(result.Output) || strings.Contains(result.Output, "END_OF_PATCH") {
			t.Fatalf("patch truncation was not reported: %+v %+v", out, result)
		}
		gitInspectWorktreeContains(t, result.Output, "@@", "+patch line")
	}
}

func TestGitInspect_WorktreeDoesNotTraverseSymlinkAncestors(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	outside := t.TempDir()
	gitInspectTestWrite(t, outside, "f.txt", "EXTERNAL_FILE_CONTENT\n")
	gitInspectTestWrite(t, outside, "EXTERNAL_ENTRY", "not listed\n")
	if err := os.RemoveAll(filepath.Join(ws.Root(), "docs")); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"docs", "untracked-link"} {
		if err := os.Symlink(outside, filepath.Join(ws.Root(), name)); err != nil {
			t.Fatal(err)
		}
	}
	for _, args := range []string{`{"operation":"status"}`, `{"operation":"worktree_diff"}`} {
		_, result := gitInspectWorktreeResult(t, ws, args)
		gitInspectWorktreeContains(t, result.Output, "docs/f.txt", "symlink", "untracked-link")
		if strings.Contains(result.Output, "EXTERNAL_") {
			t.Fatalf("symlink traversal exposed external files: %s", result.Output)
		}
	}
}
