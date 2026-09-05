package reagent

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func matchLocations(matches []searchMatch) string {
	var out []string
	for _, m := range matches {
		out = append(out, fmt.Sprintf("%s:%d", m.Path, m.Line))
	}
	return strings.Join(out, " ")
}

func TestSearchText_LiteralCaseSensitiveMatches(t *testing.T) {
	ws := testWorkspace(t, map[string]string{
		"a.txt":     "timeout here\nTIMEOUT shouted\ntimeout timeout twice\n",
		"sub/b.txt": "no match\ntimeout again\n",
	})
	var got searchTextResult
	data(t, exec(t, NewSearchTextTool(ws), `{"path":".","query":"timeout"}`), &got)

	// One match per matching line, in traversal order, case-sensitive.
	if want := "a.txt:1 a.txt:3 sub/b.txt:2"; matchLocations(got.Matches) != want {
		t.Fatalf("got %q, want %q", matchLocations(got.Matches), want)
	}
	if !got.Complete || got.StopReason != nil || got.FilesScanned != 2 {
		t.Fatalf("got %+v", got)
	}
}

// A regular expression is searched as the literal text it is made of.
func TestSearchText_DoesNotInterpretRegularExpressions(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "a.c\nabc\n"})
	var got searchTextResult
	data(t, exec(t, NewSearchTextTool(ws), `{"path":".","query":"a.c"}`), &got)

	if matchLocations(got.Matches) != "a.txt:1" {
		t.Fatalf("got %q", matchLocations(got.Matches))
	}
}

func TestSearchText_ZeroMatchesIsComplete(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "nothing here\n"})
	var got searchTextResult
	data(t, exec(t, NewSearchTextTool(ws), `{"path":".","query":"absent"}`), &got)

	if len(got.Matches) != 0 || !got.Complete {
		t.Fatalf("got %+v", got)
	}
}

// An excluded directory is skipped while walking, but searched when the model
// names it as the root (v1 §11.2).
func TestSearchText_ExcludedDirectoryIsReachableAsARoot(t *testing.T) {
	ws := testWorkspace(t, map[string]string{
		"a.txt":                  "target\n",
		"node_modules/dep/x.txt": "target\n",
	})
	tool := NewSearchTextTool(ws)

	var walked searchTextResult
	data(t, exec(t, tool, `{"path":".","query":"target"}`), &walked)
	if matchLocations(walked.Matches) != "a.txt:1" {
		t.Fatalf("walk reached an excluded directory: %q", matchLocations(walked.Matches))
	}

	var direct searchTextResult
	data(t, exec(t, tool, `{"path":"node_modules","query":"target"}`), &direct)
	if matchLocations(direct.Matches) != "node_modules/dep/x.txt:1" {
		t.Fatalf("got %q", matchLocations(direct.Matches))
	}
}

func TestSearchText_MaxResultsReportsIncompleteness(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "hit\nhit\nhit\n"})
	outcome := exec(t, NewSearchTextTool(ws), `{"path":".","query":"hit","max_results":2}`)

	var got searchTextResult
	data(t, outcome, &got)
	if len(got.Matches) != 2 || got.Complete {
		t.Fatalf("got %+v", got)
	}
	if got.StopReason == nil || *got.StopReason != "max_results" {
		t.Fatalf("got stop reason %v", got.StopReason)
	}
}

// An unreadable file is an error when the model names it, and a counted skip
// when the walk merely passes it.
func TestSearchText_UnreadableFileErrorsWhenNamedAndSkipsWhenWalked(t *testing.T) {
	ws := testWorkspace(t, map[string]string{
		"a.txt":  "target\n",
		"binary": "before\x00target",
	})
	tool := NewSearchTextTool(ws)

	if got := exec(t, tool, `{"path":"binary","query":"target"}`); got.Code != "binary_file" {
		t.Fatalf("got %s (%s)", got.Code, got.Message)
	}

	var walked searchTextResult
	data(t, exec(t, tool, `{"path":".","query":"target"}`), &walked)
	if walked.SkippedFiles != 1 || walked.Complete {
		t.Fatalf("a skipped file must make the search incomplete: %+v", walked)
	}
}

func TestSearchText_TrimsToTheResultBudget(t *testing.T) {
	line := strings.Repeat("y", 900) + " hit\n"
	ws := testWorkspace(t, map[string]string{"a.txt": strings.Repeat(line, 100)})
	outcome := exec(t, NewSearchTextTool(ws), `{"path":".","query":"hit"}`)

	var got searchTextResult
	data(t, outcome, &got)
	if len(got.Matches) == 0 || len(got.Matches) >= 100 {
		t.Fatalf("got %d matches, want a trimmed set", len(got.Matches))
	}
	if got.Complete || got.StopReason == nil || *got.StopReason != "result_bytes" {
		t.Fatalf("got %+v", got)
	}
	if len(outcome.Data) > MaxResultBytes {
		t.Fatalf("result is %d bytes, over the budget", len(outcome.Data))
	}
}

// One oversized match is shortened rather than dropped, so the model still
// learns where it is (v0 §5).
func TestSearchText_ShortensAnOversizedMatch(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "hit" + strings.Repeat("z", MaxResultBytes*2)})
	var got searchTextResult
	data(t, exec(t, NewSearchTextTool(ws), `{"path":".","query":"hit"}`), &got)

	if len(got.Matches) != 1 || !got.Matches[0].PreviewTruncated {
		t.Fatalf("got %+v", got.Matches)
	}
	if got.Matches[0].Line != 1 {
		t.Fatalf("the shortened match lost its location: %+v", got.Matches[0])
	}
}

func TestSearchText_InvalidQueries(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	for name, args := range map[string]string{
		"empty":      `{"path":".","query":""}`,
		"newline":    `{"path":".","query":"a\nb"}`,
		"nul byte":   `{"path":".","query":"a\u0000b"}`,
		"no query":   `{"path":"."}`,
		"zero limit": `{"path":".","query":"x","max_results":0}`,
	} {
		t.Run(name, func(t *testing.T) {
			if got := exec(t, NewSearchTextTool(ws), args); got.Code != "invalid_arguments" {
				t.Fatalf("got %s (%s)", got.Code, got.Message)
			}
		})
	}
}

func TestSearchText_SearchesOneNamedFile(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "hit\nmiss\n", "b.txt": "hit\n"})
	var got searchTextResult
	data(t, exec(t, NewSearchTextTool(ws), `{"path":"a.txt","query":"hit"}`), &got)

	if matchLocations(got.Matches) != "a.txt:1" || got.FilesScanned != 1 || !got.Complete {
		t.Fatalf("got %+v", got)
	}
}

// A symlink could point at matching text and v0 does not follow it, so the
// search says it did not see everything.
func TestSearchText_SymlinkIsSkippedAndCounted(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "hit\n"})
	if err := os.Symlink(filepath.Join(ws.Root(), "a.txt"), filepath.Join(ws.Root(), "link.txt")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	var got searchTextResult
	data(t, exec(t, NewSearchTextTool(ws), `{"path":".","query":"hit"}`), &got)

	if got.SkippedFiles != 1 || got.Complete {
		t.Fatalf("got %+v", got)
	}
}

// Finding exactly max_results matches still means more could exist unseen.
func TestSearchText_ExactMaxResultsIsStillIncomplete(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "hit\nhit\n"})
	var got searchTextResult
	data(t, exec(t, NewSearchTextTool(ws), `{"path":".","query":"hit","max_results":2}`), &got)

	if len(got.Matches) != 2 || got.Complete {
		t.Fatalf("got %+v", got)
	}
}

func TestSearchText_MalformedArguments(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	if got := exec(t, NewSearchTextTool(ws), `{"path":".",`); got.Code != "invalid_arguments" {
		t.Fatalf("got %s", got.Code)
	}
}

func TestSearchText_RejectsEscapingPath(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	if got := exec(t, NewSearchTextTool(ws), `{"path":"../outside","query":"x"}`); got.Code != "invalid_path" {
		t.Fatalf("got %s (%s)", got.Code, got.Message)
	}
}
