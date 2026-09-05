package reagent

import (
	"context"
	"encoding/json"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// excludedDirs are skipped by a recursive search (v1 §11.2). Selecting one of
// them as the search root still searches it.
var excludedDirs = map[string]bool{
	"node_modules": true, "vendor": true, ".venv": true,
	"venv": true, "dist": true, "build": true,
}

// searchTextTool finds literal matches so the model can choose which ranges are
// worth reading. Its central obligation is to say when it did not see
// everything, so a bounded search is never mistaken for an exhaustive one.
type searchTextTool struct{ ws *Workspace }

// NewSearchTextTool returns the literal text search tool.
func NewSearchTextTool(ws *Workspace) Tool { return searchTextTool{ws} }

type searchTextArgs struct {
	Path       string          `json:"path"`
	Query      string          `json:"query"`
	MaxResults json.RawMessage `json:"max_results"`
}

type searchMatch struct {
	Path             string `json:"path"`
	Line             int    `json:"line"`
	Text             string `json:"text"`
	PreviewTruncated bool   `json:"preview_truncated"`
}

type searchTextResult struct {
	Matches      []searchMatch `json:"matches"`
	FilesScanned int           `json:"files_scanned"`
	BytesScanned int           `json:"bytes_scanned"`
	SkippedFiles int           `json:"skipped_files"`
	Complete     bool          `json:"complete"`
	StopReason   *string       `json:"stop_reason"`
}

func (searchTextTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "search_text",
		Description: "Search a workspace file or directory for a literal, case-sensitive string. " +
			"Not a regular expression. Returns one match per matching line with its path and line number. " +
			"max_results defaults to as many matches as fit in one result. " +
			"complete reports whether the whole scope was searched; when it is false, do not " +
			"conclude the query is absent.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative file or directory, or . for the root."},
    "query": {"type": "string", "description": "Literal case-sensitive single-line query."},
    "max_results": {"type": "integer", "minimum": 1, "description": "Maximum matching lines. Defaults to as many as fit."}
  },
  "required": ["path", "query"],
  "additionalProperties": false
}`),
		Effect: EffectClassRead,
	}
}

func (t searchTextTool) Execute(_ context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a searchTextArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	maxResults, bad := optionalInt(a.MaxResults, "max_results", unlimited, 1)
	if bad != nil {
		return *bad, nil
	}
	switch {
	case a.Query == "":
		return failOutcome("invalid_arguments", "query must not be empty"), nil
	case strings.ContainsAny(a.Query, "\n\r\x00"):
		return failOutcome("invalid_arguments", "query must be one line without NUL bytes"), nil
	}
	abs, bad := t.ws.resolve(a.Path)
	if bad != nil {
		return *bad, nil
	}
	info, err := os.Stat(abs)
	if err != nil {
		return *osOutcome(err), nil
	}

	s := &scan{query: a.Query, maxResults: maxResults, complete: true}
	if info.IsDir() {
		t.walk(abs, s)
	} else {
		// An explicitly named file reports why it could not be read, rather
		// than being silently skipped the way a walked file is (v1 §12.3).
		snap, bad := readSnapshot(abs)
		if bad != nil {
			return *bad, nil
		}
		s.scanFile(t.ws.relative(abs), snap)
	}
	return s.outcome()
}

// walk searches a directory tree in name order, skipping what it cannot read
// and recording that it did so.
func (t searchTextTool) walk(root string, s *scan) {
	filepath.WalkDir(root, func(abs string, entry fs.DirEntry, err error) error {
		switch {
		case err != nil:
			s.skip()
			return nil
		case entry.IsDir():
			if abs != root && (entry.Name() == ".git" || excludedDirs[entry.Name()]) {
				return fs.SkipDir
			}
			return nil
		case !entry.Type().IsRegular():
			// A symlink could point at matching text, and v0 does not follow it.
			s.skip()
			return nil
		}
		snap, bad := readSnapshot(abs)
		if bad != nil {
			s.skip()
			return nil
		}
		s.scanFile(t.ws.relative(abs), snap)
		if s.full() {
			return filepath.SkipAll
		}
		return nil
	})
}

// scan accumulates one search. complete stays true only while nothing that
// could have held a match went unseen.
type scan struct {
	query      string
	maxResults int
	matches    []searchMatch
	files      int
	bytes      int
	skipped    int
	complete   bool
	stopReason string
}

func (s *scan) full() bool {
	return s.maxResults != unlimited && len(s.matches) >= s.maxResults
}

func (s *scan) skip() {
	s.skipped++
	s.complete = false
}

// scanFile records one match per matching line, however often the query occurs
// within that line.
func (s *scan) scanFile(path string, snap *snapshot) {
	s.files++
	s.bytes += snap.size
	for i, line := range snap.lines {
		if s.full() {
			s.stop("max_results")
			return
		}
		if strings.Contains(line, s.query) {
			s.matches = append(s.matches, searchMatch{Path: path, Line: i + 1, Text: line})
		}
	}
	if s.full() {
		s.stop("max_results")
	}
}

func (s *scan) stop(reason string) {
	s.complete = false
	if s.stopReason == "" {
		s.stopReason = reason
	}
}

func (s *scan) result(matches []searchMatch) searchTextResult {
	var reason *string
	if s.stopReason != "" {
		reason = &s.stopReason
	}
	return searchTextResult{
		Matches: matches, FilesScanned: s.files, BytesScanned: s.bytes,
		SkippedFiles: s.skipped, Complete: s.complete, StopReason: reason,
	}
}

// outcome trims the matches to the result budget and reports the trimming, so
// a shortened search never looks like an exhaustive one.
func (s *scan) outcome() (ToolOutcome, error) {
	if s.matches == nil {
		s.matches = []searchMatch{}
	}
	fit := func() []searchMatch {
		return s.matches[:fitElements(len(s.matches), func(n int) any { return s.result(s.matches[:n]) })]
	}
	shown := fit()
	if len(shown) < len(s.matches) {
		s.stop("result_bytes")
		// Recorded flags change the encoded size, so measure again with them set.
		shown = fit()
	}
	if len(shown) == 0 && len(s.matches) > 0 {
		shown = s.shrinkFirst()
	}

	outcome, err := okOutcome(s.result(shown))
	if err != nil {
		return ToolOutcome{}, err
	}
	outcome.Truncated = len(shown) < len(s.matches)
	return outcome, nil
}

// shrinkFirst shortens one oversized match rather than dropping it, so the
// model still learns where the match is (v0 §5).
func (s *scan) shrinkFirst() []searchMatch {
	only := s.matches[0]
	fits := func(m searchMatch) bool { return fitsInResult(s.result([]searchMatch{m})) }
	for len(only.Text) > 0 && !fits(only) {
		only.Text = truncateUTF8(only.Text, len(only.Text)/2)
		only.PreviewTruncated = true
	}
	if !fits(only) {
		return []searchMatch{}
	}
	return []searchMatch{only}
}
