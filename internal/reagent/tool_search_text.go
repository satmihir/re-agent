package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

// excludedDirs are skipped by a recursive search (v1 §11.2). Selecting one of
// them as the search root still searches it.
var excludedDirs = map[string]bool{
	"node_modules": true, "vendor": true, ".venv": true,
	"venv": true, "dist": true, "build": true,
}

// searchTextTool finds matches so the model can choose which ranges are worth
// reading. Its central obligation is to say when it did not see everything, so
// a bounded search is never mistaken for an exhaustive one.
type searchTextTool struct{ ws *Workspace }

// NewSearchTextTool returns the text search tool, which is literal by default.
func NewSearchTextTool(ws *Workspace) Tool { return searchTextTool{ws} }

func (t searchTextTool) withWorkspace(ws *Workspace) Tool {
	t.ws = ws
	return t
}

type searchTextArgs struct {
	Path       string          `json:"path"`
	Paths      json.RawMessage `json:"paths"`
	Query      string          `json:"query"`
	Regex      json.RawMessage `json:"regex"`
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
		Description: "Search one workspace path or up to 20 paths for a literal, case-sensitive string, or with `regex` true, " +
			"a Go RE2 regular expression matched against each line (use `|` for alternatives and `(?i)` to ignore case). " +
			"Returns one match per matching line with its path and line number. " +
			"max_results defaults to as many matches as fit in one result. " +
			"complete reports whether the whole scope was searched; when it is false, do not " +
			"conclude the query is absent.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "One workspace-relative file or directory, or . for the root; use exactly one of path or paths."},
    "paths": {"type": "array", "minItems": 1, "maxItems": 20, "items": {"type": "string"}, "description": "Up to 20 workspace-relative files or directories, searched in order; use exactly one of path or paths."},
    "query": {"type": "string", "description": "Single-line query: a literal string, or a pattern when regex is true."},
    "regex": {"type": "boolean", "description": "Treat query as a regular expression. Defaults to false."},
    "max_results": {"type": "integer", "minimum": 1, "description": "Maximum matching lines. Defaults to as many as fit."}
  },
  "required": ["query"],
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
	regex, bad := optionalBool(a.Regex, "regex")
	if bad != nil {
		return *bad, nil
	}
	switch {
	case a.Query == "":
		return failOutcome("invalid_arguments", "query must not be empty"), nil
	case strings.ContainsAny(a.Query, "\n\r\x00"):
		return failOutcome("invalid_arguments", "query must be one line without NUL bytes"), nil
	}
	match := func(line string) bool { return strings.Contains(line, a.Query) }
	// v0 §5 amendment (2026-09-25): reject regexes that could match every line.
	if regex {
		re, err := regexp.Compile(a.Query)
		if err != nil {
			return failOutcome("invalid_arguments", "regex does not compile: "+err.Error()), nil
		}
		if re.MatchString("") {
			return failOutcome("invalid_arguments", "pattern matches the empty string, so it would match every line"), nil
		}
		match = re.MatchString
	}
	var fields map[string]json.RawMessage
	json.Unmarshal(args, &fields)
	_, hasPath := fields["path"]
	hasPaths := len(a.Paths) != 0
	if hasPath == hasPaths {
		return failOutcome("invalid_arguments", "provide exactly one of path or paths"), nil
	}
	paths := []string{a.Path}
	if hasPaths {
		var raw []json.RawMessage
		if json.Unmarshal(a.Paths, &raw) != nil || len(raw) == 0 || len(raw) > 20 {
			return failOutcome("invalid_arguments", "paths must be an array of 1 to 20 strings"), nil
		}
		paths = nil
		for i, item := range raw {
			var path string
			if string(item) == "null" || json.Unmarshal(item, &path) != nil {
				return failOutcome("invalid_arguments", fmt.Sprintf("paths[%d]: path must be a string", i)), nil
			}
			paths = append(paths, path)
		}
	}
	// Validate the entire scope before reading any of its files.
	var targets []string
	var directories []bool
	for i, path := range paths {
		abs, bad := t.ws.resolve(path)
		if bad != nil {
			if hasPaths {
				bad.Message = fmt.Sprintf("paths[%d]: %s", i, bad.Message)
			}
			return *bad, nil
		}
		info, err := os.Stat(abs)
		if err != nil {
			bad := osOutcome(err)
			if hasPaths {
				bad.Message = fmt.Sprintf("paths[%d]: %s", i, bad.Message)
			}
			return *bad, nil
		}
		targets = append(targets, abs)
		directories = append(directories, info.IsDir())
	}

	s := &scan{workspace: t.ws.Root(), match: match, maxResults: maxResults, complete: true, seen: make(map[string]bool)}
	for i, abs := range targets {
		if s.full() {
			break
		}
		if directories[i] {
			t.walk(abs, s)
			continue
		}
		// Explicitly named files report read errors instead of silent skips (v1 §12.3).
		canonical, err := filepath.EvalSymlinks(abs)
		if err != nil {
			return *osOutcome(err), nil
		}
		if s.alreadySeen(canonical) {
			continue
		}
		snap, bad := readSnapshot(abs)
		if bad != nil {
			return *bad, nil
		}
		s.scanFile(t.ws.relative(abs), snap)
	}
	outcome, err := s.outcome()
	// v0 §5: a literal "|" that finds nothing was meant as alternatives every
	// time it occurred in real use; say why, without changing the search.
	if err == nil && !regex && len(s.matches) == 0 && strings.Contains(a.Query, "|") {
		outcome.Message = literalPipeHint
	}
	return outcome, err
}

const literalPipeHint = `no matches; the query contains "|" but regex is false, so it was searched literally; set regex: true to search alternatives`

// walk searches a directory tree in name order, skipping what it cannot read
// and recording that it did so.
func (t searchTextTool) walk(root string, s *scan) {
	// WalkDir does not follow directory symlinks; resolve this root only once.
	canonicalRoot, err := filepath.EvalSymlinks(root)
	if err != nil {
		s.skip()
		return
	}
	filepath.WalkDir(root, func(abs string, entry fs.DirEntry, err error) error {
		switch {
		case err != nil:
			s.skip()
			return nil
		case entry.IsDir():
			if abs != root && (withheld(entry.Name()) || excludedDirs[entry.Name()]) {
				return fs.SkipDir
			}
			return nil
		case withheld(entry.Name()):
			// Passed over uncounted, the same way a .git directory is.
			return nil
		case !entry.Type().IsRegular():
			// A symlink could point at matching text, and v0 does not follow it.
			s.skip()
			return nil
		}
		rel, err := filepath.Rel(root, abs)
		if err != nil {
			s.skip()
			return nil
		}
		if s.alreadySeen(filepath.Join(canonicalRoot, rel)) {
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
	workspace  string
	match      func(string) bool
	maxResults int
	matches    []searchMatch
	files      int
	bytes      int
	skipped    int
	complete   bool
	stopReason string
	seen       map[string]bool
}

func (s *scan) alreadySeen(canonical string) bool {
	if s.seen[canonical] {
		return true
	}
	s.seen[canonical] = true
	return false
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
	s.bytes += len(snap.content)
	for i, line := range snap.lines {
		if s.full() {
			s.stop("max_results")
			return
		}
		if s.match(line) {
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
		return s.matches[:fitElements(len(s.matches), func(n int) any { return s.result(s.matches[:n]) }, s.workspace)]
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

	outcome, err := workspaceOutcome(s.result(shown), s.workspace)
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
	fits := func(m searchMatch) bool { return fitsInResult(s.result([]searchMatch{m}), s.workspace) }
	for len(only.Text) > 0 && !fits(only) {
		only.Text = truncateUTF8(only.Text, len(only.Text)/2)
		only.PreviewTruncated = true
	}
	if !fits(only) {
		return []searchMatch{}
	}
	return []searchMatch{only}
}
