package reagent

import (
	"context"
	"encoding/json"
	"fmt"
)

// readFileTool returns numbered lines plus the digest of the whole file, which
// is what a later edit uses to prove it read the current bytes.
type readFileTool struct{ ws *Workspace }

// NewReadFileTool returns the file reading tool.
func NewReadFileTool(ws *Workspace) Tool { return readFileTool{ws} }

type readFileArgs struct {
	Path      string          `json:"path"`
	StartLine json.RawMessage `json:"start_line"`
	MaxLines  json.RawMessage `json:"max_lines"`
}

type readLine struct {
	Number int    `json:"number"`
	Text   string `json:"text"`
}

type readFileResult struct {
	Path       string     `json:"path"`
	SHA256     string     `json:"sha256"`
	SizeBytes  int        `json:"size_bytes"`
	TotalLines int        `json:"total_lines"`
	Lines      []readLine `json:"lines"`
	NextLine   *int       `json:"next_line"`
	EOF        bool       `json:"eof"`
}

func (readFileTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "read_file",
		Description: "Read a range of lines from a UTF-8 workspace file. Lines are one-based. " +
			"start_line defaults to 1 and max_lines to as many lines as fit in one result; " +
			"use next_line to continue. Returns the SHA-256 digest of the whole file. " +
			"Read the relevant range before editing it.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative UTF-8 text file."},
    "start_line": {"type": "integer", "minimum": 1, "description": "One-based first line to read. Defaults to 1."},
    "max_lines": {"type": "integer", "minimum": 1, "description": "Maximum lines to return. Defaults to as many as fit."}
  },
  "required": ["path"],
  "additionalProperties": false
}`),
		Effect: EffectClassRead,
	}
}

func (t readFileTool) Execute(_ context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a readFileArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	start, bad := optionalInt(a.StartLine, "start_line", 1, 1)
	if bad != nil {
		return *bad, nil
	}
	maxLines, bad := optionalInt(a.MaxLines, "max_lines", unlimited, 1)
	if bad != nil {
		return *bad, nil
	}
	abs, bad := t.ws.resolve(a.Path)
	if bad != nil {
		return *bad, nil
	}
	snap, bad := readSnapshot(abs)
	if bad != nil {
		return *bad, nil
	}

	total := len(snap.lines)
	// Reading line 1 of an empty file is the one range past the end that is not
	// an error, because it truthfully reports a file with no lines.
	if start > total && !(start == 1 && total == 0) {
		return failOutcome("line_out_of_range", fmt.Sprintf("file has %d lines", total)), nil
	}

	// The evidence this result may carry, before trimming to the byte budget.
	wanted := snap.lines[min(start-1, total):]
	if maxLines != unlimited && len(wanted) > maxLines {
		wanted = wanted[:maxLines]
	}
	build := func(n int) any { return readFileData(t.ws.relative(abs), snap, start, wanted[:n]) }
	shown := fitElements(len(wanted), build)
	if shown == 0 && len(wanted) > 0 {
		return failOutcome("line_too_long", fmt.Sprintf("line %d alone does not fit in one result", start)), nil
	}

	outcome, err := okOutcome(build(shown))
	if err != nil {
		return ToolOutcome{}, err
	}
	outcome.Truncated = start-1+shown < total
	return outcome, nil
}

// readFileData numbers the returned lines and recomputes the continuation
// metadata for exactly those lines, so next_line and eof always agree with what
// the result contains.
func readFileData(path string, snap *snapshot, start int, lines []string) readFileResult {
	numbered := make([]readLine, len(lines))
	for i, text := range lines {
		numbered[i] = readLine{Number: start + i, Text: text}
	}
	end := start - 1 + len(lines)
	var next *int
	if end < len(snap.lines) {
		after := end + 1
		next = &after
	}
	return readFileResult{
		Path: path, SHA256: snap.sha256, SizeBytes: len(snap.content), TotalLines: len(snap.lines),
		Lines: numbered, NextLine: next, EOF: end == len(snap.lines),
	}
}
