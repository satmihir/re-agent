package reagent

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"
)

// testWorkspace builds a throwaway workspace from a path-to-content map.
func testWorkspace(t *testing.T, files map[string]string) *Workspace {
	t.Helper()
	root := t.TempDir()
	for name, content := range files {
		path := filepath.Join(root, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	ws, err := OpenWorkspace(root)
	if err != nil {
		t.Fatal(err)
	}
	return ws
}

// exec runs a tool and fails the test on an unexpected implementation error.
func runTool(t *testing.T, tool Tool, args string) ToolOutcome {
	t.Helper()
	outcome, err := tool.Execute(context.Background(), json.RawMessage(args))
	if err != nil {
		t.Fatalf("unexpected tool error: %v", err)
	}
	return outcome
}

// data decodes a successful outcome's payload, failing on any error code.
func data(t *testing.T, outcome ToolOutcome, dst any) {
	t.Helper()
	if !outcome.OK {
		t.Fatalf("outcome %s: %s", outcome.Code, outcome.Message)
	}
	if err := json.Unmarshal(outcome.Data, dst); err != nil {
		t.Fatal(err)
	}
}

func TestWorkspace_RejectsPathsOutsideTheContract(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	cases := map[string]string{
		"empty":         "",
		"absolute":      "/etc/passwd",
		"parent":        "../secrets",
		"nested parent": "sub/../../secrets",
		"git":           ".git/config",
		"git deep":      "sub/.git/config",
		"nul":           "a\x00b",
	}
	for name, path := range cases {
		t.Run(name, func(t *testing.T) {
			if _, bad := ws.resolve(path); bad == nil || bad.Code != "invalid_path" {
				t.Fatalf("got %v, want invalid_path", bad)
			}
		})
	}
}

func TestSplitLines_CountsLinesTheWayTheDesignSays(t *testing.T) {
	cases := []struct {
		content string
		want    []string
	}{
		{"", nil},
		{"a", []string{"a"}},
		{"a\n", []string{"a"}},
		{"a\n\n", []string{"a", ""}},
		{"\n", []string{""}},
		{"a\r\nb", []string{"a", "b"}},
	}
	for _, c := range cases {
		got := splitLines([]byte(c.content))
		if len(got) != len(c.want) {
			t.Fatalf("%q: got %d lines %q, want %d", c.content, len(got), got, len(c.want))
		}
		for i := range got {
			if got[i] != c.want[i] {
				t.Fatalf("%q line %d: got %q, want %q", c.content, i, got[i], c.want[i])
			}
		}
	}
}

func TestTruncateUTF8_NeverSplitsARune(t *testing.T) {
	cases := []struct {
		in    string
		limit int
		want  string
	}{
		{"abc", 10, "abc"},
		{"abc", 3, "abc"},
		{"abcdef", 3, "abc"},
		{"héllo", 2, "h"}, // cutting inside é backs up to the rune start
		{"日本語", 4, "日"},   // 3 bytes per rune: 4 backs up to 3
		{"日本語", 2, ""},
	}
	for _, c := range cases {
		if got := truncateUTF8(c.in, c.limit); got != c.want {
			t.Fatalf("truncateUTF8(%q, %d) = %q, want %q", c.in, c.limit, got, c.want)
		}
	}
}

func TestOpenWorkspace_RejectsMissingAndNonDirectories(t *testing.T) {
	root := t.TempDir()
	file := filepath.Join(root, "a.txt")
	if err := os.WriteFile(file, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	for name, path := range map[string]string{
		"missing":   filepath.Join(root, "absent"),
		"is a file": file,
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := OpenWorkspace(path); err == nil {
				t.Fatal("want an error")
			}
		})
	}
}

// A named pipe is refused before it is opened, because opening one with no
// writer would block the run rather than fail it.
func TestReadSnapshot_NamedPipeDoesNotBlock(t *testing.T) {
	pipe := filepath.Join(t.TempDir(), "fifo")
	if err := syscall.Mkfifo(pipe, 0o600); err != nil {
		t.Skipf("named pipes unavailable: %v", err)
	}
	done := make(chan *ToolOutcome, 1)
	go func() {
		_, bad := readSnapshot(pipe)
		done <- bad
	}()
	select {
	case bad := <-done:
		if bad == nil || bad.Code != "not_file" {
			t.Fatalf("got %v, want not_file", bad)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("readSnapshot blocked on a named pipe")
	}
}
