package reagent

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"unicode/utf8"
)

// listFilesTool shows one directory at a time, so discovery never dumps a whole
// repository into the context.
type listFilesTool struct{ ws *Workspace }

// NewListFilesTool returns the directory listing tool.
func NewListFilesTool(ws *Workspace) Tool { return listFilesTool{ws} }

type listFilesArgs struct {
	Path   string          `json:"path"`
	Offset json.RawMessage `json:"offset"`
	Limit  json.RawMessage `json:"limit"`
}

type listEntry struct {
	Path string `json:"path"`
	Name string `json:"name"`
	Kind string `json:"kind"`
}

type listFilesResult struct {
	Path           string      `json:"path"`
	Entries        []listEntry `json:"entries"`
	NextOffset     *int        `json:"next_offset"`
	SkippedEntries int         `json:"skipped_entries"`
	Complete       bool        `json:"complete"`
}

func (listFilesTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "list_files",
		Description: "List one workspace directory in name order. Does not recurse. " +
			"offset defaults to 0 and limit to as many entries as fit in one result; " +
			"use next_offset to continue. Paths are workspace-relative and .git is excluded.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative directory, or . for the root."},
    "offset": {"type": "integer", "minimum": 0, "description": "Zero-based offset into the sorted listing. Defaults to 0."},
    "limit": {"type": "integer", "minimum": 1, "description": "Maximum entries to return. Defaults to as many as fit."}
  },
  "required": ["path"],
  "additionalProperties": false
}`),
		Effect: EffectClassRead,
	}
}

func (t listFilesTool) Execute(_ context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a listFilesArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	offset, bad := optionalInt(a.Offset, "offset", 0, 0)
	if bad != nil {
		return *bad, nil
	}
	limit, bad := optionalInt(a.Limit, "limit", unlimited, 1)
	if bad != nil {
		return *bad, nil
	}
	abs, bad := t.ws.resolve(a.Path)
	if bad != nil {
		return *bad, nil
	}
	if info, err := os.Stat(abs); err != nil {
		return *osOutcome(err), nil
	} else if !info.IsDir() {
		return failOutcome("not_directory", "path is not a directory"), nil
	}

	// os.ReadDir sorts by name, which is the byte order v1 §11.2 asks for.
	dir, err := os.ReadDir(abs)
	if err != nil {
		return *osOutcome(err), nil
	}

	entries, skipped := make([]listEntry, 0, len(dir)), 0
	for _, d := range dir {
		if d.Name() == ".git" || !utf8.ValidString(d.Name()) {
			skipped++
			continue
		}
		entries = append(entries, listEntry{
			Path: t.ws.relative(filepath.Join(abs, d.Name())),
			Name: d.Name(),
			Kind: entryKind(d.Type()),
		})
	}

	page := entries[min(offset, len(entries)):]
	if limit != unlimited && len(page) > limit {
		page = page[:limit]
	}
	build := func(n int) any { return listFilesData(a.Path, entries, offset, page[:n], skipped) }
	shown := page[:fitElements(len(page), build)]

	outcome, err := okOutcome(build(len(shown)))
	if err != nil {
		return ToolOutcome{}, err
	}
	outcome.Truncated = offset+len(shown) < len(entries)
	return outcome, nil
}

// listFilesData recomputes the continuation offset for whatever page survived
// trimming, so next_offset always agrees with the entries actually returned.
func listFilesData(path string, all []listEntry, offset int, page []listEntry, skipped int) listFilesResult {
	var next *int
	if end := offset + len(page); end < len(all) {
		next = &end
	}
	return listFilesResult{
		Path: path, Entries: page, NextOffset: next, SkippedEntries: skipped,
		// The directory was enumerated in full; pagination alone does not make
		// a listing incomplete (v1 §12.1).
		Complete: true,
	}
}

func entryKind(mode os.FileMode) string {
	switch {
	case mode&os.ModeSymlink != 0:
		return "symlink"
	case mode.IsDir():
		return "dir"
	default:
		return "file"
	}
}
