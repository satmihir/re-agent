package reagent

import (
	"bytes"
	"fmt"
	"math/rand"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestGitInspectUnified_Golden(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, old, current, want string
	}{
		{"unchanged", "same\n", "same\n", ""},
		{"empty", "", "", ""},
		{"add", "", "new\n", "@@ -0,0 +1 @@\n+new\n"},
		{"delete", "old\n", "", "@@ -1 +0,0 @@\n-old\n"},
		{"add_unterminated", "", "new", "@@ -0,0 +1 @@\n+new\n\\ No newline at end of file\n"},
		{"delete_unterminated", "old", "", "@@ -1 +0,0 @@\n-old\n\\ No newline at end of file\n"},
		{"add_newline", "same", "same\n", "@@ -1 +1 @@\n-same\n\\ No newline at end of file\n+same\n"},
		{"remove_newline", "same\n", "same", "@@ -1 +1 @@\n-same\n+same\n\\ No newline at end of file\n"},
		{"both_unterminated", "old", "new", "@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n"},
		{"unterminated_context", "old\ntail", "new\ntail", "@@ -1,2 +1,2 @@\n-old\n+new\n tail\n\\ No newline at end of file\n"},
		{"blank_line", "", "\n", "@@ -0,0 +1 @@\n+\n"},
		{"crlf", "old\r\n", "new\r\n", "@@ -1 +1 @@\n-old\r\n+new\r\n"},
		{"ambiguous_swap", "a\nb\n", "b\na\n", "@@ -1,2 +1,2 @@\n-a\n b\n+a\n"},
		{"equal_predecessor_reach", "a\nb\n", "c\na\n", "@@ -1,2 +1,2 @@\n+c\n a\n-b\n"},
		{"repeated_lines", "a\nb\na\n", "a\na\nb\n", "@@ -1,3 +1,3 @@\n a\n-b\n a\n+b\n"},
		{"moved_block", "head\none\ntwo\nthree\nfour\ntail\n", "head\nthree\nfour\none\ntwo\ntail\n", "@@ -1,6 +1,6 @@\n head\n-one\n-two\n three\n four\n+one\n+two\n tail\n"},
		{
			"merge_six_context_lines",
			"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n",
			"ONE\n2\n3\n4\n5\n6\n7\nEIGHT\n9\n10\n11\n12\n",
			"@@ -1,11 +1,11 @@\n-1\n+ONE\n 2\n 3\n 4\n 5\n 6\n 7\n-8\n+EIGHT\n 9\n 10\n 11\n",
		},
		{
			"split_seven_context_lines",
			"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n",
			"ONE\n2\n3\n4\n5\n6\n7\n8\nNINE\n10\n11\n12\n",
			"@@ -1,4 +1,4 @@\n-1\n+ONE\n 2\n 3\n 4\n@@ -6,7 +6,7 @@\n 6\n 7\n 8\n-9\n+NINE\n 10\n 11\n 12\n",
		},
		{
			"second_hunk_line_offset",
			"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n",
			"zero\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\nlast\n",
			"@@ -1,3 +1,4 @@\n+zero\n 1\n 2\n 3\n@@ -9,4 +10,4 @@\n 9\n 10\n 11\n-12\n+last\n",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			want := tc.want
			if want != "" {
				want = "--- a/f\n+++ b/f\n" + want
			}
			for i := 0; i < 3; i++ {
				got, ok := gitInspectUnified([]byte(tc.old), []byte(tc.current), "a/f", "b/f")
				if !ok || got != want {
					t.Fatalf("ok=%v\ngot:\n%s\nwant:\n%s", ok, got, want)
				}
			}
		})
	}
}

func TestGitInspectUnified_Bounds(t *testing.T) {
	t.Parallel()
	longOld := bytes.Repeat(append(bytes.Repeat([]byte{'x'}, 8190), 'a', '\n'), 64)
	longNew := bytes.Repeat(append(bytes.Repeat([]byte{'x'}, 8190), 'b', '\n'), 64)
	for _, tc := range []struct {
		name         string
		old, current []byte
		ok           bool
	}{
		{"distance_at_limit", nil, bytes.Repeat([]byte("x\n"), 512), true},
		{"distance_over_limit", nil, bytes.Repeat([]byte("x\n"), 513), false},
		{"replace_at_limit", bytes.Repeat([]byte("x\n"), 256), bytes.Repeat([]byte("y\n"), 256), true},
		{"replace_over_limit", bytes.Repeat([]byte("x\n"), 257), bytes.Repeat([]byte("y\n"), 256), false},
		{"work_limit", longOld, longNew, false},
		{"bytes_at_limit", bytes.Repeat([]byte{'x'}, 1<<20), bytes.Repeat([]byte{'y'}, 1<<20), true},
		{"old_bytes_over_limit", make([]byte, 1<<20+1), nil, false},
		{"new_bytes_over_limit", nil, make([]byte, 1<<20+1), false},
		{"unchanged_over_limit", make([]byte, 1<<20+1), make([]byte, 1<<20+1), false},
		{"many_lines", bytes.Repeat([]byte{'\n'}, 1<<20), append(bytes.Repeat([]byte{'\n'}, 1<<20-2), 'x', '\n'), true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			patch, ok := gitInspectUnified(tc.old, tc.current, "a/f", "b/f")
			if ok != tc.ok || (!ok && patch != "") || (ok && patch == "") {
				t.Fatalf("ok=%v, want %v, patch bytes=%d", ok, tc.ok, len(patch))
			}
		})
	}
}

func TestGitInspectUnified_DevNullNames(t *testing.T) {
	t.Parallel()
	got, ok := gitInspectUnified(nil, []byte("new\n"), "/dev/null", "b/f")
	if !ok || got != "--- /dev/null\n+++ b/f\n@@ -0,0 +1 @@\n+new\n" {
		t.Fatalf("ok=%v, patch=%q", ok, got)
	}
}

func TestGitInspectUnified_HeaderNamesAreQuotedAndApply(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, path, quoted string }{
		{"plain", "plain.txt", "a/plain.txt"},
		{"space", "with space", "a/with space"},
		{"unicode", "café", `"a/caf\303\251"`},
		{"escape", "with\x1bescape", `"a/with\033escape"`},
		{"control", "with\x01control", `"a/with\001control"`},
		{"separator", "with\u2028separator", `"a/with\342\200\250separator"`},
		{"newline", "line\n@@ injected @@", `"a/line\n@@ injected @@"`},
		{"tab", "with\ttab", `"a/with\ttab"`},
		{"backslash", "with\\slash", `"a/with\\slash"`},
		{"double_quote", "with\"quote", `"a/with\"quote"`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			patch, ok := gitInspectUnified([]byte("old\n"), []byte("new\n"), "a/"+tc.path, "b/"+tc.path)
			newName := strings.Replace(tc.quoted, "a/", "b/", 1)
			want := "--- " + tc.quoted + "\n+++ " + newName + "\n@@ -1 +1 @@\n-old\n+new\n"
			if !ok || patch != want {
				t.Fatalf("ok=%v, patch=%q, want=%q", ok, patch, want)
			}
			root := t.TempDir()
			name := filepath.Join(root, tc.path)
			if err := os.WriteFile(name, []byte("old\n"), 0600); err != nil {
				t.Fatal(err)
			}
			cmd := exec.Command("git", "apply", "--no-index", "--unsafe-paths", "--whitespace=nowarn", "-")
			cmd.Dir = root
			cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "HOME=" + root, "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL=" + os.DevNull}
			cmd.Stdin = strings.NewReader(patch)
			if out, err := cmd.CombinedOutput(); err != nil {
				t.Fatalf("git apply: %v: %s\n%s", err, out, patch)
			}
			got, err := os.ReadFile(name)
			if err != nil || string(got) != "new\n" {
				t.Fatalf("patched=%q, err=%v", got, err)
			}
		})
	}
}

func TestGitInspectUnified_PatchAppliesAndHasMinimalDistance(t *testing.T) {
	t.Parallel()
	rng := rand.New(rand.NewSource(136))
	root := t.TempDir()
	name := filepath.Join(root, "f")
	for sample := 0; sample < 120; sample++ {
		var old, current []string
		for _, lines := range []*[]string{&old, &current} {
			for n := rng.Intn(16); n > 0; n-- {
				*lines = append(*lines, fmt.Sprintf("%c\n", 'a'+rng.Intn(4)))
			}
			if len(*lines) > 0 && rng.Intn(2) == 0 {
				i := len(*lines) - 1
				(*lines)[i] = strings.TrimSuffix((*lines)[i], "\n")
			}
		}
		before, after := strings.Join(old, ""), strings.Join(current, "")
		patch, ok := gitInspectUnified([]byte(before), []byte(after), "a/f", "b/f")
		if !ok {
			t.Fatalf("sample %d exceeded bound", sample)
		}
		// An independent dynamic-programming oracle protects shortest-edit behavior.
		distance := make([][]int, len(old)+1)
		for i := range distance {
			distance[i] = make([]int, len(current)+1)
			for j := range distance[i] {
				switch {
				case i == 0:
					distance[i][j] = j
				case j == 0:
					distance[i][j] = i
				case old[i-1] == current[j-1]:
					distance[i][j] = distance[i-1][j-1]
				default:
					distance[i][j] = 1 + min(distance[i-1][j], distance[i][j-1])
				}
			}
		}
		changes := 0
		for _, line := range strings.Split(patch, "\n") {
			if strings.HasPrefix(line, "+") && !strings.HasPrefix(line, "+++") || strings.HasPrefix(line, "-") && !strings.HasPrefix(line, "---") {
				changes++
			}
		}
		if changes != distance[len(old)][len(current)] {
			t.Fatalf("sample %d: changes=%d, minimum=%d\n%s", sample, changes, distance[len(old)][len(current)], patch)
		}
		if patch == "" {
			if before != after {
				t.Fatalf("sample %d: empty patch changes content", sample)
			}
			continue
		}
		if err := os.WriteFile(name, []byte(before), 0600); err != nil {
			t.Fatal(err)
		}
		cmd := exec.Command("git", "apply", "--no-index", "--unsafe-paths", "--whitespace=nowarn", "-")
		cmd.Dir = root
		cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "HOME=" + root, "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL=" + os.DevNull}
		cmd.Stdin = strings.NewReader(patch)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("sample %d: git apply: %v: %s\n%s", sample, err, out, patch)
		}
		got, err := os.ReadFile(name)
		if err != nil || string(got) != after {
			t.Fatalf("sample %d: patched=%q, want=%q, err=%v", sample, got, after, err)
		}
	}
}
