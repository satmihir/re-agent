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
	OldText string `json:"old_text"`
	NewText string `json:"new_text"`
}

type editFileResult struct {
	Operation     string `json:"operation"`
	Path          string `json:"path"`
	Changed       bool   `json:"changed"`
	BeforeSHA256  string `json:"before_sha256"`
	AfterSHA256   string `json:"after_sha256"`
	SizeBytes     int    `json:"size_bytes"`
	Edits         int    `json:"edits,omitempty"`
	AppendedBytes int    `json:"appended_bytes,omitempty"`
}

func (editFileTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "edit_file",
		Description: "Edit a UTF-8 workspace file after reading it; pass its expected_sha256. " +
			"Use edits for several changes to one file in one call, or old_text/new_text for one. " +
			"Each old_text must occur exactly once in the original snapshot; matches must not overlap. " +
			"Use append_text to add to the end (start with a newline if the file does not end with one). " +
			"No change publishes on failure. Never edit files through exec scripts: only edit_file checks " +
			"the digest and matches. Empty new_text deletes a match. Cannot create or delete files; unavailable in read-only mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative UTF-8 text file to change."},
    "expected_sha256": {"type": "string", "description": "The digest read_file returned for the whole file."},
    "old_text": {"type": "string", "description": "Single edit: exact text occurring once in the file."},
    "new_text": {"type": "string", "description": "Single edit: replacement text; empty deletes."},
    "edits": {"type": "array", "description": "Several non-overlapping replacements in one call.", "minItems": 1, "items": {
      "type": "object", "properties": {
        "old_text": {"type": "string"}, "new_text": {"type": "string"}
      }, "required": ["old_text", "new_text"], "additionalProperties": false
    }},
    "append_text": {"type": "string", "description": "Text added after all replacements; no automatic newline."}
  },
  "required": ["path", "expected_sha256"],
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
	if !digestPattern.MatchString(a.ExpectedSHA256) {
		return failOutcome("invalid_arguments", invalidExpectedDigest), nil
	}
	var fields map[string]json.RawMessage
	json.Unmarshal(args, &fields)
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
	snap, bad := t.ws.checkFileDigest(abs, a.ExpectedSHA256)
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
		switch occurrences(before, edit.OldText) {
		case 0:
			return failOutcome("edit_not_found", label+"old_text does not occur in the file"), nil
		case 1:
		default:
			return failOutcome("ambiguous_edit", label+"old_text occurs more than once; include enough context to make it unique"), nil
		}
		start := strings.Index(before, edit.OldText)
		matches = append(matches, match{start, start + len(edit.OldText), i})
	}
	sort.Slice(matches, func(i, j int) bool { return matches[i].start < matches[j].start })
	var builder strings.Builder
	from := 0
	for j, m := range matches {
		if m.start < from {
			return failOutcome("invalid_arguments", fmt.Sprintf("edits[%d] and edits[%d]: matched ranges overlap", matches[j-1].index, m.index)), nil
		}
		builder.WriteString(before[from:m.start])
		builder.WriteString(edits[m.index].NewText)
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

	result := editFileResult{
		Operation: "update", Path: path, Changed: true,
		BeforeSHA256: snap.sha256, AfterSHA256: digestOf(after), SizeBytes: len(after),
		Edits: len(edits), AppendedBytes: len(appendText),
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
		edits = append(edits, fileEdit{a.OldText, a.NewText})
	} else if multi {
		var items []json.RawMessage
		if json.Unmarshal(a.Edits, &items) != nil || len(items) == 0 {
			return nil, "", failPtr("invalid_arguments", "edits must be a non-empty array")
		}
		for i, item := range items {
			var raw struct {
				OldText json.RawMessage `json:"old_text"`
				NewText json.RawMessage `json:"new_text"`
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
		if edit.OldText == edit.NewText {
			return nil, "", failPtr("invalid_arguments", prefix+"old_text and new_text are the same, so the edit changes nothing")
		}
	}
	return edits, appendText, nil
}

// occurrences counts matches including overlapping ones, so a query like "aa"
// in "aaa" is reported as ambiguous rather than silently replaced once.
func occurrences(content, query string) int {
	count, from := 0, 0
	for {
		at := strings.Index(content[from:], query)
		if at < 0 {
			return count
		}
		count++
		if count > 1 {
			return count
		}
		from += at + 1
	}
}
