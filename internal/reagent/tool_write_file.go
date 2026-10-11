package reagent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"
)

// v0 §8 amendment (2026-09-27): create and delete join guarded file editing.
type writeFileTool struct {
	ws     *Workspace
	agents bool
}
type deleteFileTool struct {
	ws     *Workspace
	remove func(string) error
}

const invalidExpectedDigest = "expected_sha256 must be a 64-character digest, or omitted to use the version you last read or wrote"

// NewWriteFileTool returns the whole-file creation and replacement tool.
func NewWriteFileTool(ws *Workspace) Tool { return writeFileTool{ws: ws} }

// NewDeleteFileTool returns guarded file and opt-in recursive deletion.
func NewDeleteFileTool(ws *Workspace) Tool { return deleteFileTool{ws: ws} }

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
	Path           string          `json:"path"`
	ExpectedSHA256 json.RawMessage `json:"expected_sha256"`
	Recursive      json.RawMessage `json:"recursive"`
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
			"Replacing requires reading or writing that file in this conversation; read it again after changes outside these tools (exec or the user). " +
			"Prefer edit_file for changes to part of a file. Unavailable in read-only mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative UTF-8 text file to create or replace."},
    "content": {"type": "string", "description": "Complete new UTF-8 text for the file."},
    "expected_sha256": {"type": "string", "description": "Optional. Omit it to use the version you last read or wrote; pass a digest only to name a specific version."}
  },
  "required": ["path", "content"],
  "additionalProperties": false
}`),
		Effect: EffectClassWrite,
	}
}

func (deleteFileTool) Spec() ToolSpec {
	return ToolSpec{
		Name:        "delete_file",
		Description: "Delete one regular file by its last read/write digest, or an explicit expected_sha256. Explicit digests permit unread binary files of any size; exec-created files get no digest exemption. Set recursive:true to authorize a directory tree or symlink deletion without prior reads; symlinks are removed, never followed. Refuses the workspace root, symlink ancestors and withheld names anywhere in the tree. Stops at the first failure, reporting actual removed paths (bounded with omitted count) as an applied effect. File digest rules still apply with recursive:true. Unavailable in read-only mode.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "path": {"type": "string", "description": "Workspace-relative regular file, or directory/symlink with recursive:true."},
    "expected_sha256": {"type": "string", "description": "Optional regular-file SHA-256; omit to use the last observed version. Not accepted for directories or symlinks."},
    "recursive": {"type": "boolean", "description": "Explicitly authorize a directory tree or symlink deletion; default false."}
  },
  "required": ["path"],
  "additionalProperties": false
}`),
		Effect: EffectClassWrite,
	}
}

func (t writeFileTool) Execute(ctx context.Context, args json.RawMessage) (ToolOutcome, error) {
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
	expected, bad := expectedDigest(a.ExpectedSHA256)
	if bad != nil {
		return *bad, nil
	}
	hasDigest := len(a.ExpectedSHA256) != 0
	t.ws.publishMu.Lock()
	defer t.ws.publishMu.Unlock()
	if ctx.Err() != nil {
		return failOutcome("not_executed", "cancelled before file publication"), nil
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
		if t.agents {
			t.ws.mu.Lock()
			_, previouslySeen := t.ws.seen[missingPath(abs)]
			t.ws.mu.Unlock()
			if previouslySeen {
				return failOutcome("stale_file", "the file you read or wrote was deleted; read_file must observe its absence before recreating it"), nil
			}
		}
		// v0 §8: preserve existing-parent behavior; create only missing parents.
		parent := filepath.Dir(abs)
		parentInfo, err := os.Stat(parent)
		var created []string
		if errors.Is(err, os.ErrNotExist) {
			created, bad = t.createParents(parent)
			if bad != nil {
				return *bad, nil
			}
		} else if err != nil {
			return *osOutcome(err), nil
		} else if !parentInfo.IsDir() {
			return failOutcome("not_directory", "parent path is not a directory"), nil
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
				return failOutcome("invalid_arguments", "the file exists; read it before changing it"), nil
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
	var snap *snapshot
	if hasDigest {
		snap, bad = t.ws.checkFileDigest(abs, expected)
	} else {
		snap, bad = t.ws.checkSeenDigest(abs)
	}
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

// v0 §8: missing parents cannot be created through symlinked components.
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

func (t deleteFileTool) Execute(ctx context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a deleteFileArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	expected, bad := expectedDigest(a.ExpectedSHA256)
	if bad != nil {
		return *bad, nil
	}
	recursive, bad := optionalBool(a.Recursive, "recursive")
	if bad != nil {
		return *bad, nil
	}
	t.ws.publishMu.Lock()
	defer t.ws.publishMu.Unlock()
	if ctx.Err() != nil {
		return failOutcome("not_executed", "cancelled before deletion"), nil
	}
	var abs string
	if recursive {
		abs, bad = t.deletePath(a.Path)
	} else {
		abs, bad = t.ws.resolve(a.Path)
	}
	if bad != nil {
		return *bad, nil
	}
	info, err := os.Lstat(abs)
	if err != nil {
		return *osOutcome(err), nil
	}
	if recursive && (info.IsDir() || info.Mode()&os.ModeSymlink != 0) {
		if len(a.ExpectedSHA256) != 0 {
			return failOutcome("invalid_arguments", "expected_sha256 is only valid for regular files"), nil
		}
		return t.deleteTree(ctx, abs)
	}
	if bad := fileTarget(info); bad != nil {
		return *bad, nil
	}
	var before string
	if len(a.ExpectedSHA256) != 0 {
		before, bad = t.deleteDigest(ctx, abs, expected, info)
	} else {
		var snap *snapshot
		snap, bad = t.ws.checkSeenDigest(abs)
		if bad == nil {
			before = snap.sha256
		}
	}
	if bad != nil {
		return *bad, nil
	}
	if ctx.Err() != nil {
		return failOutcome("not_executed", "cancelled before deletion"), nil
	}
	current, err := os.Lstat(abs)
	if err != nil {
		return *osOutcome(err), nil
	}
	if !os.SameFile(info, current) || info.Mode().Type() != current.Mode().Type() {
		return failOutcome("stale_file", "path changed while preparing deletion"), nil
	}
	canonical, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return *osOutcome(err), nil
	}
	if err := t.removePath(abs); err != nil {
		return *osOutcome(err), nil
	}
	t.forgetDeleted(canonical, false)
	return appliedOutcome(deleteFileResult{Operation: "delete", Path: t.ws.relative(abs), BeforeSHA256: before}, t.ws.Root())
}

func (t deleteFileTool) removePath(path string) error {
	if t.remove != nil {
		return t.remove(path)
	}
	return os.Remove(path)
}

func (t deleteFileTool) deleteDigest(ctx context.Context, abs, expected string, info os.FileInfo) (string, *ToolOutcome) {
	f, err := os.Open(abs)
	if err != nil {
		return "", osOutcome(err)
	}
	defer f.Close()
	opened, err := f.Stat()
	if err != nil {
		return "", osOutcome(err)
	}
	if !opened.Mode().IsRegular() || !os.SameFile(info, opened) {
		return "", failPtr("stale_file", "path changed while preparing deletion")
	}
	hash := sha256.New()
	buffer := make([]byte, 64*1024)
	for {
		if ctx.Err() != nil {
			return "", failPtr("not_executed", "cancelled while hashing file")
		}
		n, err := f.Read(buffer)
		hash.Write(buffer[:n])
		if err == io.EOF {
			break
		}
		if err != nil {
			return "", osOutcome(err)
		}
	}
	before := hex.EncodeToString(hash.Sum(nil))
	if before == expected {
		return before, nil
	}
	if t.ws.returned(abs, expected) {
		return "", failPtr("stale_file", "the file has changed since it was read; its current digest is "+before)
	}
	return "", failPtr("unknown_digest", "this is not a digest read_file returned for this file; its current digest is "+before)
}

// Recursive deletion validates ancestors without resolving a link leaf.
func (t deleteFileTool) deletePath(path string) (string, *ToolOutcome) {
	if !utf8.ValidString(path) || strings.ContainsRune(path, 0) {
		return "", failPtr("invalid_path", "path must be valid UTF-8 without NUL bytes")
	}
	for _, part := range strings.Split(filepath.ToSlash(path), "/") {
		if part == ".." || withheld(part) {
			return "", failPtr("invalid_path", "path contains a parent or withheld component")
		}
	}
	abs := filepath.Clean(path)
	if !filepath.IsAbs(abs) {
		abs = filepath.Join(t.ws.Root(), abs)
	}
	if !insidePath(t.ws.Root(), abs) || abs == t.ws.Root() {
		return "", failPtr("invalid_path", "deletion must stay inside the workspace and cannot remove its root")
	}
	rel, err := filepath.Rel(t.ws.Root(), abs)
	if err != nil {
		return "", osOutcome(err)
	}
	parts := strings.Split(rel, string(filepath.Separator))
	current := t.ws.Root()
	for i, part := range parts {
		current = filepath.Join(current, part)
		info, err := os.Lstat(current)
		if err != nil {
			return "", osOutcome(err)
		}
		if i == len(parts)-1 {
			break
		}
		if info.Mode()&os.ModeSymlink != 0 {
			return "", failPtr("symlink_target", "recursive deletion refuses symlink ancestors")
		}
		if !info.IsDir() {
			return "", failPtr("not_directory", "parent path is not a directory")
		}
	}
	return abs, nil
}

type deleteTreeResult struct {
	Operation    string   `json:"operation"`
	Path         string   `json:"path"`
	Recursive    bool     `json:"recursive"`
	RemovedPaths []string `json:"removed_paths"`
	RemovedCount int      `json:"removed_count"`
	Omitted      int      `json:"omitted"`
	FailedPath   string   `json:"failed_path,omitempty"`
}

type deleteEntry struct {
	path string
	info os.FileInfo
}

func (t deleteFileTool) deleteTree(ctx context.Context, abs string) (ToolOutcome, error) {
	var plan []deleteEntry
	var problem *ToolOutcome
	// Refuse withheld names before removing anything, including on partial failure.
	err := filepath.WalkDir(abs, func(path string, entry fs.DirEntry, walkErr error) error {
		if ctx.Err() != nil {
			problem = failPtr("not_executed", "cancelled before deletion")
			return fs.SkipAll
		}
		if walkErr != nil {
			problem = osOutcome(walkErr)
			return fs.SkipAll
		}
		if _, bad := t.deletePath(path); bad != nil {
			problem = bad
			return fs.SkipAll
		}
		info, err := os.Lstat(path)
		if err != nil {
			problem = osOutcome(err)
			return fs.SkipAll
		}
		if info.Mode().Type() != entry.Type() {
			problem = failPtr("stale_file", "path changed during preflight")
			return fs.SkipAll
		}
		if !info.IsDir() && !info.Mode().IsRegular() && info.Mode()&os.ModeSymlink == 0 {
			problem = failPtr("invalid_arguments", "recursive deletion refuses special files")
			return fs.SkipAll
		}
		plan = append(plan, deleteEntry{path, info})
		return nil
	})
	if err != nil {
		return *osOutcome(err), nil
	}
	if problem != nil {
		return *problem, nil
	}
	result := deleteTreeResult{Operation: "delete", Path: t.ws.relative(abs), Recursive: true, RemovedPaths: []string{}}
	bytes := 0
	for i := len(plan) - 1; i >= 0; i-- {
		entry := plan[i]
		if ctx.Err() != nil {
			problem = failPtr("not_executed", "cancelled during deletion")
		}
		if problem == nil {
			_, problem = t.deletePath(entry.path)
		}
		if problem == nil {
			current, err := os.Lstat(entry.path)
			if err != nil {
				problem = osOutcome(err)
			} else if !os.SameFile(entry.info, current) || entry.info.Mode().Type() != current.Mode().Type() {
				problem = failPtr("stale_file", "path changed after preflight")
			}
		}
		if problem == nil {
			if err := t.removePath(entry.path); err != nil {
				problem = osOutcome(err)
			}
		}
		if problem != nil {
			result.FailedPath = t.ws.relative(entry.path)
			break
		}
		t.forgetDeleted(entry.path, entry.info.IsDir())
		result.RemovedCount++
		rel := t.ws.relative(entry.path)
		raw, _ := json.Marshal(rel)
		if bytes+len(raw)+1 <= 8*1024 {
			result.RemovedPaths = append(result.RemovedPaths, rel)
			bytes += len(raw) + 1
		}
	}
	result.Omitted = result.RemovedCount - len(result.RemovedPaths)
	out, err := workspaceOutcome(result, t.ws.Root())
	if err != nil {
		return ToolOutcome{}, err
	}
	if result.RemovedCount > 0 {
		out.Effect = EffectApplied
	}
	if problem != nil {
		out.OK = false
		out.Code = problem.Code
		out.Message = truncateUTF8(problem.Message, 256)
	}
	// Deep paths and a long workspace root also consume the envelope budget.
	for encodedSize(out) > MaxResultBytes && len(result.RemovedPaths) > 0 {
		result.RemovedPaths = result.RemovedPaths[:len(result.RemovedPaths)-1]
		result.Omitted++
		out.Data, _ = json.Marshal(result)
	}
	out.Truncated = result.Omitted > 0
	return out, nil
}

func (t deleteFileTool) forgetDeleted(path string, directory bool) {
	t.ws.mu.Lock()
	defer t.ws.mu.Unlock()
	for seen := range t.ws.seen {
		if seen == path || directory && insidePath(path, seen) {
			delete(t.ws.seen, seen)
		}
	}
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

// v0 §8: an omitted digest uses the latest version returned to this conversation.
func (w *Workspace) checkSeenDigest(abs string) (*snapshot, *ToolOutcome) {
	snap, bad := readSnapshot(abs)
	if bad != nil {
		return nil, bad
	}
	canonical, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return nil, osOutcome(err)
	}
	w.mu.Lock()
	expected, ok := w.seen[canonical]
	w.mu.Unlock()
	if !ok {
		return nil, failPtr("invalid_arguments", "read the file before changing it (read_file records what you have seen)")
	}
	if expected != snap.sha256 {
		return nil, failPtr("stale_file", "the file has changed since you last read or wrote it (for example through exec); its current digest is "+snap.sha256+". Read the part you will change again, or pass expected_sha256 if you know what changed")
	}
	return snap, nil
}

func expectedDigest(raw json.RawMessage) (string, *ToolOutcome) {
	if len(raw) == 0 {
		return "", nil
	}
	var expected string
	if json.Unmarshal(raw, &expected) != nil || !digestPattern.MatchString(expected) {
		return "", failPtr("invalid_arguments", invalidExpectedDigest)
	}
	return expected, nil
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
