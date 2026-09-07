package reagent

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func listedNames(entries []listEntry) string {
	var names []string
	for _, e := range entries {
		names = append(names, e.Name)
	}
	return strings.Join(names, ",")
}

func TestListFiles_SortedAndPaginated(t *testing.T) {
	ws := testWorkspace(t, map[string]string{
		"b.txt": "x", "a.txt": "x", "c.txt": "x", "sub/d.txt": "x",
	})
	tool := NewListFilesTool(ws)

	var first listFilesResult
	data(t, runTool(t, tool, `{"path":".","limit":2}`), &first)
	if got := listedNames(first.Entries); got != "a.txt,b.txt" {
		t.Fatalf("got %q, want the first two names in order", got)
	}
	if first.NextOffset == nil || *first.NextOffset != 2 || !first.Complete {
		t.Fatalf("got %+v", first)
	}

	var second listFilesResult
	data(t, runTool(t, tool, `{"path":".","offset":2}`), &second)
	if got := listedNames(second.Entries); got != "c.txt,sub" {
		t.Fatalf("got %q", got)
	}
	if second.NextOffset != nil {
		t.Fatalf("the last page must not offer a next offset: %+v", second)
	}
	if second.Entries[1].Kind != "dir" || second.Entries[0].Kind != "file" {
		t.Fatalf("got kinds %+v", second.Entries)
	}
}

func TestListFiles_ExcludesGitAndCountsIt(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x", ".git/config": "x"})
	var got listFilesResult
	data(t, runTool(t, NewListFilesTool(ws), `{"path":"."}`), &got)

	if listedNames(got.Entries) != "a.txt" || got.SkippedEntries != 1 {
		t.Fatalf("got %+v", got)
	}
}

func TestListFiles_OffsetBeyondEndIsAnEmptyPage(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	outcome := runTool(t, NewListFilesTool(ws), `{"path":".","offset":50}`)

	var got listFilesResult
	data(t, outcome, &got)
	if len(got.Entries) != 0 || got.NextOffset != nil || outcome.Truncated {
		t.Fatalf("got %+v truncated=%v", got, outcome.Truncated)
	}
}

func TestListFiles_Errors(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	cases := map[string]struct{ args, code string }{
		"missing":       {`{"path":"absent"}`, "not_found"},
		"not directory": {`{"path":"a.txt"}`, "not_directory"},
		"git":           {`{"path":".git"}`, "invalid_path"},
		"unknown field": {`{"path":".","depth":2}`, "invalid_arguments"},
		"zero limit":    {`{"path":".","limit":0}`, "invalid_arguments"},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			if got := runTool(t, NewListFilesTool(ws), c.args); got.OK || got.Code != c.code {
				t.Fatalf("got %s (%s), want %s", got.Code, got.Message, c.code)
			}
		})
	}
}

func TestListFiles_ReportsSymlinkKind(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	if err := os.Symlink(filepath.Join(ws.Root(), "a.txt"), filepath.Join(ws.Root(), "link")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	var got listFilesResult
	data(t, runTool(t, NewListFilesTool(ws), `{"path":"."}`), &got)

	if len(got.Entries) != 2 || got.Entries[1].Kind != "symlink" {
		t.Fatalf("got %+v", got.Entries)
	}
}
