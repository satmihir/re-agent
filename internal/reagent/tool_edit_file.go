package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"
)

// editFileTool replaces one exact, uniquely matching piece of text in one file.
//
// The digest the model supplies is the whole safety mechanism: it proves the
// model read the bytes it is editing, so an edit written against a stale view
// is refused rather than applied to something else (v1 §13.3).
type editFileTool struct{ ws *Workspace }

// NewEditFileTool returns the file editing tool. It is registered only in write
// mode; the human grants that at launch (v1 §10.3).
func NewEditFileTool(ws *Workspace) Tool { return editFileTool{ws} }

type editFileArgs struct {
	Path           string `json:"path"`
	ExpectedSHA256 string `json:"expected_sha256"`
	OldText        string `json:"old_text"`
	NewText        string `json:"new_text"`
}

type editFileResult struct {
	Operation    string `json:"operation"`
	Path         string `json:"path"`
	Changed      bool   `json:"changed"`
	BeforeSHA256 string `json:"before_sha256"`
	AfterSHA256  string `json:"after_sha256"`
	SizeBytes    int    `json:"size_bytes"`
}

func (editFileTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "edit_file",
		Description: "Replace one exact piece of text in a UTF-8 workspace file. " +
			"Read the file first and pass the SHA-256 digest read_file returned; a stale digest " +
			"is refused without changing anything. old_text must occur exactly once and is matched " +
			"byte for byte, with no whitespace or case normalization. An empty new_text deletes the " +
			"matched text. Creating and deleting files is not supported. Requires write mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative UTF-8 text file to change."},
    "expected_sha256": {"type": "string", "description": "The digest read_file returned for the whole file."},
    "old_text": {"type": "string", "description": "Exact text occurring once in the file."},
    "new_text": {"type": "string", "description": "Replacement text. Empty removes the matched text."}
  },
  "required": ["path", "expected_sha256", "old_text", "new_text"],
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
	switch {
	case !digestPattern.MatchString(a.ExpectedSHA256):
		return failOutcome("invalid_arguments", "expected_sha256 must be 64 lowercase hex characters"), nil
	case a.OldText == "":
		return failOutcome("invalid_arguments", "old_text must not be empty"), nil
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
	// bytes compared are exactly the bytes edited.
	snap, bad := readSnapshot(abs)
	if bad != nil {
		return *bad, nil
	}
	if snap.sha256 != a.ExpectedSHA256 {
		return failOutcome("stale_file",
			"the file has changed since it was read; its current digest is "+snap.sha256), nil
	}

	before := string(snap.content)
	switch occurrences(before, a.OldText) {
	case 0:
		return failOutcome("edit_not_found", "old_text does not occur in the file"), nil
	case 1:
	default:
		return failOutcome("ambiguous_edit", "old_text occurs more than once; include enough context to make it unique"), nil
	}
	after := strings.Replace(before, a.OldText, a.NewText, 1)

	switch {
	case len(after) > MaxFileBytes:
		return failOutcome("file_too_large", fmt.Sprintf("the result would exceed the %d byte limit", MaxFileBytes)), nil
	case strings.ContainsRune(after, 0):
		// UTF-8 is self-synchronizing and JSON strings decode to valid UTF-8,
		// so a NUL byte is the only way the result can stop being text.
		return failOutcome("binary_file", "the result would contain NUL bytes"), nil
	}

	path := t.ws.relative(abs)
	// Replacing a file with its own contents is reported, not performed.
	if after == before {
		return okOutcome(editFileResult{
			Operation: "update", Path: path, Changed: false,
			BeforeSHA256: snap.sha256, AfterSHA256: snap.sha256, SizeBytes: len(snap.content),
		})
	}
	if err := publish(abs, []byte(after), info.Mode().Perm()); err != nil {
		// A failed rename leaves the original in place, so nothing was applied.
		return *osOutcome(err), nil
	}

	outcome, err := okOutcome(editFileResult{
		Operation: "update", Path: path, Changed: true,
		BeforeSHA256: snap.sha256, AfterSHA256: digestOf(after), SizeBytes: len(after),
	})
	if err != nil {
		return ToolOutcome{}, err
	}
	outcome.Effect = EffectApplied
	return outcome, nil
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
