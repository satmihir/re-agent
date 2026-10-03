package reagent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"
)

// v0 §8 amendment (2026-09-27): create and delete join guarded file editing.
type writeFileTool struct{ ws *Workspace }
type deleteFileTool struct{ ws *Workspace }

const invalidExpectedDigest = "expected_sha256 must be the 64-character digest read_file returned"

// NewWriteFileTool returns the whole-file creation and replacement tool.
func NewWriteFileTool(ws *Workspace) Tool { return writeFileTool{ws} }

// NewDeleteFileTool returns the guarded regular-file deletion tool.
func NewDeleteFileTool(ws *Workspace) Tool { return deleteFileTool{ws} }

func (t writeFileTool) withWorkspace(ws *Workspace) Tool {
	t.ws = ws
	return t
}

func (t deleteFileTool) withWorkspace(ws *Workspace) Tool {
	t.ws = ws
	return t
}

type writeFileArgs struct {
	Path           string          `json:"path"`
	Content        json.RawMessage `json:"content"`
	ExpectedSHA256 json.RawMessage `json:"expected_sha256"`
}

type deleteFileArgs struct {
	Path           string `json:"path"`
	ExpectedSHA256 string `json:"expected_sha256"`
}

type writeFileResult struct {
	Operation    string   `json:"operation"`
	Path         string   `json:"path"`
	BeforeSHA256 *string  `json:"before_sha256"`
	AfterSHA256  string   `json:"after_sha256"`
	SizeBytes    int      `json:"size_bytes"`
	CreatedDirs  []string `json:"created_dirs,omitempty"`
}

type deleteFileResult struct {
	Operation    string `json:"operation"`
	Path         string `json:"path"`
	BeforeSHA256 string `json:"before_sha256"`
}

func (writeFileTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "write_file",
		Description: "Create a UTF-8 text file, or replace all of an existing file's content. " +
			"Creating requires the file not to exist; missing parent directories are created. " +
			"Replacing requires expected_sha256 from the latest read_file of that file. " +
			"Prefer edit_file for changes to part of a file. Unavailable in read-only mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative UTF-8 text file to create or replace."},
    "content": {"type": "string", "description": "Complete new UTF-8 text for the file."},
    "expected_sha256": {"type": "string", "description": "Digest from read_file, required to replace an existing file; omit to create."}
  },
  "required": ["path", "content"],
  "additionalProperties": false
}`),
		Effect: EffectClassWrite,
	}
}

func (deleteFileTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "delete_file",
		Description: "Delete one file. Requires expected_sha256 from the latest read_file of that file. " +
			"Unavailable in read-only mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative regular file to delete."},
    "expected_sha256": {"type": "string", "description": "Digest read_file returned for the whole file."}
  },
  "required": ["path", "expected_sha256"],
  "additionalProperties": false
}`),
		Effect: EffectClassWrite,
	}
}

func (t writeFileTool) Execute(_ context.Context, args json.RawMessage) (ToolOutcome, error) {
	// encoding/json repairs malformed UTF-8 in strings; reject it before decoding.
	if !utf8.Valid(args) {
		return failOutcome("invalid_utf8", "content is not valid UTF-8"), nil
	}
	var a writeFileArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	if len(a.Content) == 0 || string(a.Content) == "null" {
		return failOutcome("invalid_arguments", "content must be a string"), nil
	}
	var content string
	if err := json.Unmarshal(a.Content, &content); err != nil {
		return failOutcome("invalid_arguments", "content must be a string"), nil
	}
	if len(content) > MaxFileBytes {
		return failOutcome("file_too_large", fmt.Sprintf("content exceeds the %d byte limit", MaxFileBytes)), nil
	}
	if !utf8.ValidString(content) {
		return failOutcome("invalid_utf8", "content is not valid UTF-8"), nil
	}
	if strings.ContainsRune(content, 0) {
		return failOutcome("binary_file", "content contains NUL bytes"), nil
	}
	var expected string
	hasDigest := len(a.ExpectedSHA256) != 0
	if hasDigest {
		if err := json.Unmarshal(a.ExpectedSHA256, &expected); err != nil || !digestPattern.MatchString(expected) {
			return failOutcome("invalid_arguments", invalidExpectedDigest), nil
		}
	}
	abs, bad := t.ws.resolve(a.Path)
	if bad != nil {
		return *bad, nil
	}
	info, err := os.Lstat(abs)
	if errors.Is(err, os.ErrNotExist) {
		if hasDigest {
			return failOutcome("not_found", "no such path in the workspace"), nil
		}
		// v0 §8: create parents only for a new file, and undo empty ones on failure.
		created, bad := t.createParents(filepath.Dir(abs))
		if bad != nil {
			return *bad, nil
		}
		published := false
		defer func() {
			if !published {
				removeEmptyDirs(created)
			}
		}()
		// v0 §8 amendment: a create must not replace a path that appears mid-call.
		if err := publishNew(abs, []byte(content)); err != nil {
			if errors.Is(err, os.ErrExist) {
				return failOutcome("invalid_arguments", "the file exists; read it and pass its sha256 to overwrite it"), nil
			}
			return *osOutcome(err), nil
		}
		result := writeFileResult{
			Operation: "create", Path: t.ws.relative(abs), AfterSHA256: digestOf(content), SizeBytes: len(content),
		}
		for _, dir := range created {
			result.CreatedDirs = append(result.CreatedDirs, t.ws.relative(dir))
		}
		outcome, err := appliedOutcome(result, t.ws.Root())
		if err != nil {
			return ToolOutcome{}, err
		}
		t.ws.remember(abs, result.AfterSHA256)
		published = true
		return outcome, nil
	}
	if err != nil {
		return *osOutcome(err), nil
	}
	if bad := fileTarget(info); bad != nil {
		return *bad, nil
	}
	if !hasDigest {
		return failOutcome("invalid_arguments", "the file exists; read it and pass its sha256 to overwrite it"), nil
	}
	snap, bad := t.ws.checkFileDigest(abs, expected)
	if bad != nil {
		return *bad, nil
	}
	if err := publish(abs, []byte(content), info.Mode().Perm()); err != nil {
		return *osOutcome(err), nil
	}
	result := writeFileResult{
		Operation: "overwrite", Path: t.ws.relative(abs), BeforeSHA256: &snap.sha256,
		AfterSHA256: digestOf(content), SizeBytes: len(content),
	}
	outcome, err := appliedOutcome(result, t.ws.Root())
	if err != nil {
		return ToolOutcome{}, err
	}
	t.ws.remember(abs, result.AfterSHA256)
	return outcome, nil
}

// v0 §8: only create may add directories; each parent must be a real directory.
func (t writeFileTool) createParents(parent string) ([]string, *ToolOutcome) {
	rel, err := filepath.Rel(t.ws.Root(), parent)
	if err != nil || !insidePath(t.ws.Root(), parent) {
		return nil, failPtr("invalid_path", "parent must be inside the workspace")
	}
	var created []string
	current := t.ws.Root()
	if rel == "." {
		return nil, nil
	}
	for _, part := range strings.Split(filepath.ToSlash(rel), "/") {
		if withheld(part) {
			removeEmptyDirs(created)
			return nil, failPtr("invalid_path", part+" is not readable")
		}
		current = filepath.Join(current, part)
		info, err := os.Lstat(current)
		if errors.Is(err, os.ErrNotExist) {
			if err := os.Mkdir(current, 0o755); err != nil {
				removeEmptyDirs(created)
				return nil, osOutcome(err)
			}
			created = append(created, current)
			continue
		}
		if err != nil {
			removeEmptyDirs(created)
			return nil, osOutcome(err)
		}
		if info.Mode()&os.ModeSymlink != 0 {
			removeEmptyDirs(created)
			return nil, failPtr("symlink_target", "parent path is a symlink")
		}
		if !info.IsDir() {
			removeEmptyDirs(created)
			return nil, failPtr("not_directory", "parent path is not a directory")
		}
	}
	return created, nil
}

func removeEmptyDirs(created []string) {
	for i := len(created) - 1; i >= 0; i-- {
		if err := os.Remove(created[i]); err != nil {
			break
		}
	}
}

func (t deleteFileTool) Execute(_ context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a deleteFileArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	if !digestPattern.MatchString(a.ExpectedSHA256) {
		return failOutcome("invalid_arguments", invalidExpectedDigest), nil
	}
	abs, bad := t.ws.resolve(a.Path)
	if bad != nil {
		return *bad, nil
	}
	info, err := os.Lstat(abs)
	if err != nil {
		return *osOutcome(err), nil
	}
	if bad := fileTarget(info); bad != nil {
		return *bad, nil
	}
	snap, bad := t.ws.checkFileDigest(abs, a.ExpectedSHA256)
	if bad != nil {
		return *bad, nil
	}
	if err := os.Remove(abs); err != nil {
		return *osOutcome(err), nil
	}
	return appliedOutcome(deleteFileResult{Operation: "delete", Path: t.ws.relative(abs), BeforeSHA256: snap.sha256}, t.ws.Root())
}

func fileTarget(info os.FileInfo) *ToolOutcome {
	switch {
	case info.Mode()&os.ModeSymlink != 0:
		return failPtr("symlink_target", "path is a symlink")
	case !info.Mode().IsRegular():
		return failPtr("invalid_arguments", "path is not a regular file")
	}
	return nil
}

// v0 §8 amendment (2026-09-27, U3): provenance diagnoses only mismatches;
// a matching digest is valid whether or not a tool returned it before.
func (w *Workspace) checkFileDigest(abs, expected string) (*snapshot, *ToolOutcome) {
	snap, bad := readSnapshot(abs)
	if bad != nil {
		return nil, bad
	}
	if snap.sha256 == expected {
		return snap, nil
	}
	if w.returned(abs, expected) {
		return nil, failPtr("stale_file", "the file has changed since it was read; its current digest is "+snap.sha256)
	}
	return nil, failPtr("unknown_digest", "this is not a digest read_file returned for this file; its current digest is "+snap.sha256)
}

func appliedOutcome(result any, workspace string) (ToolOutcome, error) {
	outcome, err := workspaceOutcome(result, workspace)
	if err != nil {
		return ToolOutcome{}, err
	}
	outcome.Effect = EffectApplied
	return outcome, nil
}
