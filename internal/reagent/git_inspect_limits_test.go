package reagent

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"
)

func gitInspectLimitsGitInput(t *testing.T, root, input string, args ...string) string {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "git", args...)
	cmd.Dir = root
	cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "LANG=C", "LC_ALL=C", "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_SYSTEM=" + os.DevNull, "GIT_CONFIG_GLOBAL=" + os.DevNull}
	cmd.Stdin = strings.NewReader(input)
	output, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v %s", args, err, output)
	}
	return strings.TrimSpace(string(output))
}

func TestGitInspect_InventoryLimitAllowsScopedRecovery(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	oid := gitInspectTestGit(t, root, "rev-parse", "HEAD:f.txt")
	prefix := "inventory/" + strings.Repeat("a", 220) + "/" + strings.Repeat("b", 220) + "/" + strings.Repeat("c", 220) + "/" + strings.Repeat("d", 222)
	var inventory strings.Builder
	// Populate only Git objects and the index, not ten thousand working files.
	for i := 0; i < 10000; i++ {
		fmt.Fprintf(&inventory, "100644 %s\t%s%05d\n", oid, prefix, i)
	}
	if len(prefix)+5 != 900 || inventory.Len() <= 8<<20 {
		t.Fatal("fixture must exceed the inventory byte limit with 900-byte paths")
	}
	gitInspectLimitsGitInput(t, root, inventory.String(), "update-index", "--index-info")
	tree := gitInspectTestGit(t, root, "write-tree")
	parent := gitInspectTestGit(t, root, "rev-parse", "HEAD")
	commit := gitInspectTestGit(t, root, "commit-tree", tree, "-p", parent, "-m", "Large inventory")
	gitInspectTestGit(t, root, "update-ref", "HEAD", commit)
	gitInspectTestWrite(t, root, "f.txt", "SCOPED_RECOVERY_CONTENT\n")

	out, result := gitInspectWorktreeResult(t, ws, `{"operation":"status"}`)
	if !out.Truncated || !result.OutputTruncated || result.OutputBytesSeen != len(result.Output) {
		t.Fatalf("inventory truncation not reported: %+v %+v", out, result)
	}
	gitInspectWorktreeContains(t, result.Output, "inventory exceeds byte limit", "comparison unavailable")
	if strings.Contains(result.Output, "staged ") || strings.Contains(result.Output, "SCOPED_RECOVERY_CONTENT") {
		t.Fatalf("incomplete inventory produced guessed changes: %s", result.Output)
	}
	for _, operation := range []string{"status", "worktree_diff"} {
		t.Run(operation, func(t *testing.T) {
			args := fmt.Sprintf(`{"operation":%q,"paths":["f.txt"]}`, operation)
			out, result := gitInspectWorktreeResult(t, ws, args)
			if out.Truncated || result.OutputTruncated || strings.Contains(result.Output, "inventory/") {
				t.Fatalf("scoped inventory did not recover: %+v %+v", out, result)
			}
			if operation == "status" {
				gitInspectWorktreeLine(t, result.Output, "unstaged", "modified", `"f.txt"`)
			} else {
				gitInspectWorktreeContains(t, result.Output, "@@", "-after", "+SCOPED_RECOVERY_CONTENT")
			}
		})
	}
}

func TestGitInspect_StagedBinaryAndOversizeStatusSummaries(t *testing.T) {
	t.Parallel()
	for _, kind := range []string{"binary", "oversize"} {
		t.Run(kind, func(t *testing.T) {
			ws := gitInspectTestRepo(t)
			root := ws.Root()
			before, after := "BINARY_BEFORE_SECRET\x00\n", "BINARY_AFTER_SECRET\x00\n"
			if kind == "oversize" {
				before = strings.Repeat("LARGE_BEFORE_SECRET", (1<<20)/len("LARGE_BEFORE_SECRET")+1)
				after = strings.Repeat("LARGE_AFTER_SECRET", (1<<20)/len("LARGE_AFTER_SECRET")+2)
			}
			for _, name := range []string{"modified", "deleted"} {
				gitInspectTestWrite(t, root, name, before)
			}
			gitInspectTestGit(t, root, "add", "modified", "deleted")
			gitInspectTestGit(t, root, "commit", "-q", "-m", "Summary baseline")
			oldOID := gitInspectTestGit(t, root, "rev-parse", "HEAD:modified")
			for _, name := range []string{"added", "modified"} {
				gitInspectTestWrite(t, root, name, after)
			}
			gitInspectTestGit(t, root, "add", "added", "modified")
			gitInspectTestGit(t, root, "rm", "deleted")
			newOID := gitInspectTestGit(t, root, "rev-parse", ":modified")
			for _, tc := range []struct {
				name, oldOID, newOID string
				oldSize, newSize     int
			}{
				{"added", "", newOID, 0, len(after)},
				{"modified", oldOID, newOID, len(before), len(after)},
				{"deleted", oldOID, "", len(before), 0},
			} {
				t.Run(tc.name, func(t *testing.T) {
					args := fmt.Sprintf(`{"operation":"status","paths":[%q]}`, tc.name)
					out, result := gitInspectWorktreeResult(t, ws, args)
					gitInspectWorktreeLine(t, result.Output, "staged", tc.name, fmt.Sprintf("%q", tc.name))
					gitInspectWorktreeContains(t, result.Output, fmt.Sprintf("changed: old size=%d blob=%s; new size=%d blob=%s", tc.oldSize, tc.oldOID, tc.newSize, tc.newOID))
					if out.Truncated || result.OutputTruncated || strings.Contains(result.Output, "SECRET") || strings.Contains(result.Output, "\x00") || strings.Contains(result.Output, "@@") || strings.Contains(result.Output, "unstaged ") {
						t.Fatalf("staged content was emitted or misclassified: %+v %s", out, result.Output)
					}
				})
			}
		})
	}
}

func TestGitInspect_UnmergedIndexIsUnknownNotStagedModification(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	base := gitInspectTestGit(t, root, "rev-parse", "HEAD:f.txt")
	ours := gitInspectLimitsGitInput(t, root, "UNMERGED_OURS_SECRET\n", "hash-object", "-w", "--stdin")
	theirs := gitInspectLimitsGitInput(t, root, "UNMERGED_THEIRS_SECRET\n", "hash-object", "-w", "--stdin")
	input := fmt.Sprintf("0 %s\tf.txt\n100644 %s 1\tf.txt\n100644 %s 2\tf.txt\n100644 %s 3\tf.txt\n", strings.Repeat("0", 40), base, ours, theirs)
	gitInspectLimitsGitInput(t, root, input, "update-index", "--index-info")
	gitInspectTestWrite(t, root, "f.txt", "UNMERGED_WORKING_SECRET\n")
	for _, operation := range []string{"status", "worktree_diff"} {
		t.Run(operation, func(t *testing.T) {
			args := fmt.Sprintf(`{"operation":%q,"paths":["f.txt"]}`, operation)
			out, result := gitInspectWorktreeResult(t, ws, args)
			gitInspectWorktreeLine(t, result.Output, `index "f.txt"`, "unmerged index", "comparison unknown")
			if out.Truncated || result.OutputTruncated || strings.Contains(result.Output, "staged ") || strings.Contains(result.Output, "modified") || strings.Contains(result.Output, "SECRET") || strings.Contains(result.Output, "@@") {
				t.Fatalf("unmerged stages were guessed as a change: %+v %s", out, result.Output)
			}
		})
	}
}

func TestGitInspect_IndexOnlyGitlinkExcludesScopedDescendants(t *testing.T) {
	t.Parallel()
	ws := gitInspectTestRepo(t)
	root := ws.Root()
	// Base descendants force a comparison even when Git omits untracked gitlink contents.
	gitInspectTestWrite(t, root, "module/secret", "base file\n")
	gitInspectTestWrite(t, root, "module/nested/secret", "base nested file\n")
	gitInspectTestGit(t, root, "add", "module")
	gitInspectTestGit(t, root, "commit", "-q", "-m", "Ordinary directory baseline")
	base := gitInspectTestGit(t, root, "rev-parse", "HEAD")
	gitInspectTestGit(t, root, "update-index", "--force-remove", "module/secret", "module/nested/secret")
	gitInspectTestGit(t, root, "update-index", "--add", "--cacheinfo", "160000,"+base+",module")
	gitInspectTestWrite(t, root, "module/secret", "INDEX_ONLY_GITLINK_SECRET\n")
	gitInspectTestWrite(t, root, "module/nested/secret", "INDEX_ONLY_NESTED_SECRET\n")
	gitInspectWorktreeContains(t, gitInspectTestGit(t, root, "ls-tree", "-r", "HEAD", "--", "module/secret"), "100644", "module/secret")
	if gitInspectTestGit(t, root, "ls-files", "--stage", "--", "module/secret") != "" {
		t.Fatal("descendant index inventory must omit its new gitlink ancestor")
	}
	for _, operation := range []string{"status", "worktree_diff"} {
		for _, selected := range []string{"", "module", "module/secret", "module/nested/secret"} {
			t.Run(operation+"/"+selected, func(t *testing.T) {
				args := fmt.Sprintf(`{"operation":%q}`, operation)
				if selected != "" {
					args = fmt.Sprintf(`{"operation":%q,"paths":[%q]}`, operation, selected)
				}
				out, result := gitInspectWorktreeResult(t, ws, args)
				if out.Truncated || result.OutputTruncated || strings.Contains(result.Output, "secret") || strings.Contains(result.Output, "SECRET") {
					t.Fatalf("descended into index-only gitlink: %+v %s", out, result.Output)
				}
				if selected == "" || selected == "module" {
					gitInspectWorktreeLine(t, result.Output, "submodule", `"module"`, "working tree not inspected")
					if operation == "status" {
						gitInspectWorktreeLine(t, result.Output, "staged", "added", `"module"`)
					}
				} else if result.Output != "" {
					t.Fatalf("explicit descendant selection bypassed gitlink exclusion: %s", result.Output)
				}
			})
		}
	}
}
