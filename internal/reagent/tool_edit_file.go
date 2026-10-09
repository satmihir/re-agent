package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"
)

// v0 §8: digest-checked replacements and append publish as one update.
type editFileTool struct{ ws *Workspace }

// NewEditFileTool returns the file editing tool. It is registered only in write
// mode; the human grants that at launch (v1 §10.3).
func NewEditFileTool(ws *Workspace) Tool { return editFileTool{ws} }

func (t editFileTool) withWorkspace(ws *Workspace) Tool {
	t.ws = ws
	return t
}

type editFileArgs struct {
	Path           string          `json:"path"`
	ExpectedSHA256 string          `json:"expected_sha256"`
	OldText        string          `json:"old_text"`
	NewText        string          `json:"new_text"`
	Edits          json.RawMessage `json:"edits,omitempty"`
	AppendText     json.RawMessage `json:"append_text,omitempty"`
}

type fileEdit struct {
	OldText    string `json:"old_text"`
	NewText    string `json:"new_text"`
	ReplaceAll bool   `json:"replace_all,omitempty"`
}

type editFileResult struct {
	Operation     string   `json:"operation"`
	Path          string   `json:"path"`
	Changed       bool     `json:"changed"`
	BeforeSHA256  string   `json:"before_sha256"`
	AfterSHA256   string   `json:"after_sha256"`
	SizeBytes     int      `json:"size_bytes"`
	Edits         int      `json:"edits,omitempty"`
	EditCounts    []int    `json:"edit_counts,omitempty"`
	SkippedEdits  []string `json:"skipped_edits,omitempty"`
	AppendedBytes int      `json:"appended_bytes,omitempty"`
}

func (editFileTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "edit_file",
		Description: "Edit a UTF-8 workspace file that you have read or written in this conversation. Read it again after changes outside these tools (exec or the user). " +
			"Use edits for several changes to one file in one call, or old_text/new_text for one. " +
			"Each old_text must occur exactly once in the original snapshot unless its edit sets replace_all to replace every non-overlapping occurrence (at least one required). Matches across edits must not overlap. " +
			"An identity edit is checked for a unique match and skipped; all-identity calls are invalid. The result reports replacement counts per edit and skipped edits. " +
			"Use append_text to add to the end (start with a newline if the file does not end with one). " +
			"No change publishes on failure. Never edit files through exec scripts: only edit_file checks " +
			"the digest and matches. Empty new_text deletes a match. Cannot create or delete files; unavailable in read-only mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative UTF-8 text file to change."},
    "expected_sha256": {"type": "string", "description": "Optional. Omit it to use the version you last read or wrote; pass a digest only to name a specific version."},
    "old_text": {"type": "string", "description": "Single edit: exact text occurring once in the file."},
    "new_text": {"type": "string", "description": "Single edit: replacement text; empty deletes."},
    "edits": {"type": "array", "description": "Several non-overlapping replacements in one call.", "minItems": 1, "items": {
      "type": "object", "properties": {
        "old_text": {"type": "string"}, "new_text": {"type": "string"}, "replace_all": {"type": "boolean", "description": "Replace every non-overlapping occurrence of old_text."}
      }, "required": ["old_text", "new_text"], "additionalProperties": false
    }},
    "append_text": {"type": "string", "description": "Text added after all replacements; no automatic newline."}
  },
  "required": ["path"],
  "additionalProperties": false
}`),
		Effect: EffectClassWrite,
	}
}

func (t editFileTool) Execute(_ context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a editFileArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	var fields map[string]json.RawMessage
	json.Unmarshal(args, &fields)
	if _, supplied := fields["expected_sha256"]; supplied && !digestPattern.MatchString(a.ExpectedSHA256) {
		return failOutcome("invalid_arguments", invalidExpectedDigest), nil
	}
	edits, appendText, bad := a.changes(fields)
	if bad != nil {
		return *bad, nil
	}
	abs, bad := t.ws.resolve(a.Path)
	if bad != nil {
		return *bad, nil
	}
	info, err := os.Lstat(abs)
	if err != nil {
		return *osOutcome(err), nil
	}
	// A symlink target would make the edited file ambiguous (v1 §11.1).
	if info.Mode()&os.ModeSymlink != 0 {
		return failOutcome("symlink_target", "path is a symlink"), nil
	}

	// One snapshot answers both the digest check and the replacement, so the
	// bytes compared are exactly the bytes edited (v0 §8 amendment, 2026-09-27).
	var snap *snapshot
	if _, supplied := fields["expected_sha256"]; supplied {
		snap, bad = t.ws.checkFileDigest(abs, a.ExpectedSHA256)
	} else {
		snap, bad = t.ws.checkSeenDigest(abs)
	}
	if bad != nil {
		return *bad, nil
	}

	before := string(snap.content)
	// v0 §8: all ranges are located in the same digest-checked snapshot.
	type match struct{ start, end, index int }
	matches := make([]match, 0, len(edits))
	for i, edit := range edits {
		label := ""
		if len(a.Edits) != 0 {
			label = fmt.Sprintf("edits[%d]: ", i)
		}
		lines := matchLines(before, edit.OldText, nil, 10)
		switch len(lines) {
		case 0:
			message := "old_text does not occur in the file"
			normalized, offsets := normalizeEditWhitespace(before)
			query, _ := normalizeEditWhitespace(edit.OldText)
			near := matchLines(normalized, query, offsets, 5)
			if len(near) > 0 {
				message += fmt.Sprintf("; it matches with different whitespace at line %s (read that range again and copy it exactly)", formatLines(near, 5))
			} else {
				message += "; read the range again before retrying"
			}
			return failOutcome("edit_not_found", label+message), nil
		case 1:
		default:
			if edit.ReplaceAll && edit.OldText != edit.NewText {
				break
			}
			count := fmt.Sprintf("%d", len(lines))
			if len(lines) > 10 {
				count = "at least 10"
			}
			location := "at lines"
			if len(lines) > 10 {
				location = "first at lines"
			}
			return failOutcome("ambiguous_edit", label+fmt.Sprintf("old_text occurs %s times, %s %s; include enough surrounding text to make it unique or set replace_all if every occurrence should change", count, location, formatLines(lines, 10))), nil
		}
		for from := 0; ; {
			at := strings.Index(before[from:], edit.OldText)
			if at < 0 {
				break
			}
			start := from + at
			matches = append(matches, match{start, start + len(edit.OldText), i})
			if !edit.ReplaceAll {
				break
			}
			from = start + len(edit.OldText)
		}
	}
	sort.Slice(matches, func(i, j int) bool { return matches[i].start < matches[j].start })
	counts := make([]int, len(edits))
	var skipped []string
	for i, edit := range edits {
		if edit.OldText == edit.NewText {
			skipped = append(skipped, fmt.Sprintf("edits[%d] changed nothing and was skipped", i))
		}
	}
	if len(edits) > 0 && len(skipped) == len(edits) {
		return failOutcome("invalid_arguments", "every edit changes nothing"), nil
	}
	var builder strings.Builder
	from := 0
	for j, m := range matches {
		if m.start < from {
			return failOutcome("invalid_arguments", fmt.Sprintf("edits[%d] and edits[%d]: matched ranges overlap", matches[j-1].index, m.index)), nil
		}
		builder.WriteString(before[from:m.start])
		builder.WriteString(edits[m.index].NewText)
		if edits[m.index].OldText != edits[m.index].NewText {
			counts[m.index]++
		}
		from = m.end
	}
	builder.WriteString(before[from:])
	builder.WriteString(appendText)
	after := builder.String()

	switch {
	case len(after) > MaxFileBytes:
		return failOutcome("file_too_large", fmt.Sprintf("the result would exceed the %d byte limit", MaxFileBytes)), nil
	case strings.ContainsRune(after, 0):
		// UTF-8 is self-synchronizing and JSON strings decode to valid UTF-8,
		// so a NUL byte is the only way the result can stop being text.
		return failOutcome("binary_file", "the result would contain NUL bytes"), nil
	}

	path := t.ws.relative(abs)
	if err := publish(abs, []byte(after), info.Mode().Perm()); err != nil {
		// A failed rename leaves the original in place, so nothing was applied.
		return *osOutcome(err), nil
	}

	replacements := 0
	for _, count := range counts {
		replacements += count
	}
	result := editFileResult{
		Operation: "update", Path: path, Changed: true,
		BeforeSHA256: snap.sha256, AfterSHA256: digestOf(after), SizeBytes: len(after),
		Edits: replacements, EditCounts: counts, SkippedEdits: skipped, AppendedBytes: len(appendText),
	}
	outcome, err := workspaceOutcome(result, t.ws.Root())
	if err != nil {
		return ToolOutcome{}, err
	}
	outcome.Effect = EffectApplied
	t.ws.remember(abs, result.AfterSHA256)
	return outcome, nil
}

// v0 §8: reject incomplete changes before reading or publishing the file.
func (a editFileArgs) changes(fields map[string]json.RawMessage) ([]fileEdit, string, *ToolOutcome) {
	_, hasOld := fields["old_text"]
	_, hasNew := fields["new_text"]
	single := hasOld || hasNew
	multi := len(a.Edits) != 0
	if single && multi {
		return nil, "", failPtr("invalid_arguments", "old_text/new_text and edits[0] cannot be combined")
	}
	if !single && !multi && len(a.AppendText) == 0 {
		return nil, "", failPtr("invalid_arguments", "at least one change is required")
	}
	appendText := ""
	if len(a.AppendText) != 0 {
		if err := json.Unmarshal(a.AppendText, &appendText); err != nil || appendText == "" {
			return nil, "", failPtr("invalid_arguments", "append_text must be a non-empty string")
		}
	}
	var edits []fileEdit
	if single {
		if !hasOld || !hasNew {
			return nil, "", failPtr("invalid_arguments", "old_text and new_text are both required")
		}
		if string(fields["old_text"]) == "null" || string(fields["new_text"]) == "null" {
			return nil, "", failPtr("invalid_arguments", "old_text and new_text must be strings")
		}
		edits = append(edits, fileEdit{OldText: a.OldText, NewText: a.NewText})
	} else if multi {
		var items []json.RawMessage
		if json.Unmarshal(a.Edits, &items) != nil || len(items) == 0 {
			return nil, "", failPtr("invalid_arguments", "edits must be a non-empty array")
		}
		for i, item := range items {
			var raw struct {
				OldText    json.RawMessage `json:"old_text"`
				NewText    json.RawMessage `json:"new_text"`
				ReplaceAll json.RawMessage `json:"replace_all"`
			}
			if bad := decodeArgs(item, &raw); bad != nil {
				return nil, "", failPtr("invalid_arguments", fmt.Sprintf("edits[%d]: %s", i, bad.Message))
			}
			if len(raw.OldText) == 0 || len(raw.NewText) == 0 {
				return nil, "", failPtr("invalid_arguments", fmt.Sprintf("edits[%d]: old_text and new_text are both required", i))
			}
			var edit fileEdit
			if json.Unmarshal(raw.OldText, &edit.OldText) != nil || json.Unmarshal(raw.NewText, &edit.NewText) != nil || string(raw.OldText) == "null" || string(raw.NewText) == "null" {
				return nil, "", failPtr("invalid_arguments", fmt.Sprintf("edits[%d]: old_text and new_text must be strings", i))
			}
			if len(raw.ReplaceAll) != 0 && (json.Unmarshal(raw.ReplaceAll, &edit.ReplaceAll) != nil || string(raw.ReplaceAll) == "null") {
				return nil, "", failPtr("invalid_arguments", fmt.Sprintf("edits[%d]: replace_all must be a boolean", i))
			}
			edits = append(edits, edit)
		}
	}
	for i, edit := range edits {
		prefix := ""
		if multi {
			prefix = fmt.Sprintf("edits[%d]: ", i)
		}
		if edit.OldText == "" {
			return nil, "", failPtr("invalid_arguments", prefix+"old_text must not be empty")
		}
	}
	return edits, appendText, nil
}

// v0 §8: include overlapping matches, but bound error messages for large files.
func matchLines(content, query string, offsets []int, limit int) []int {
	if query == "" {
		return nil
	}
	var lines []int
	for from := 0; from < len(content); {
		at := strings.Index(content[from:], query)
		if at < 0 {
			break
		}
		start := from + at
		if offsets != nil {
			start = offsets[start]
		}
		lines = append(lines, strings.Count(content[:from+at], "\n")+1)
		if offsets != nil {
			lines[len(lines)-1] = strings.Count(content[:start], "\n") + 1
		}
		if len(lines) > limit {
			break
		}
		from += at + 1
	}
	return lines
}

func formatLines(lines []int, limit int) string {
	parts := make([]string, 0, min(len(lines), limit))
	for _, line := range lines[:min(len(lines), limit)] {
		parts = append(parts, fmt.Sprint(line))
	}
	return strings.Join(parts, ", ")
}

// The offsets retain original line positions even when whitespace collapses.
func normalizeEditWhitespace(text string) (string, []int) {
	var normalized strings.Builder
	var offsets []int
	for i := 0; i < len(text); {
		if text[i] != ' ' && text[i] != '\t' {
			normalized.WriteByte(text[i])
			offsets = append(offsets, i)
			i++
			continue
		}
		start := i
		for i < len(text) && (text[i] == ' ' || text[i] == '\t') {
			i++
		}
		if i == len(text) || text[i] == '\n' {
			continue
		}
		normalized.WriteByte(' ')
		offsets = append(offsets, start)
	}
	return normalized.String(), offsets
}
