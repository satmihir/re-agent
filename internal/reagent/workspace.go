package reagent

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"unicode/utf8"
)

// Workspace is the directory the read tools may see.
//
// Its ordinary path checks are not race-proof sandboxing; v1 §11.1 restores rooted access.
type Workspace struct {
	root    string
	mu      sync.Mutex
	digests map[string]map[string]bool
	seen    map[string]string
	// Shared by the tree's handles; observations remain conversation-local.
	publishMu *sync.Mutex
}

// OpenWorkspace opens an existing directory with a canonical immutable root.
func OpenWorkspace(path string) (*Workspace, error) {
	root, err := filepath.Abs(path)
	if err != nil {
		return nil, err
	}
	root, err = filepath.EvalSymlinks(root)
	if err != nil {
		return nil, err
	}
	info, err := os.Stat(root)
	if err != nil {
		return nil, err
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("workspace %s is not a directory", root)
	}
	return &Workspace{root: root, digests: make(map[string]map[string]bool), seen: make(map[string]string), publishMu: &sync.Mutex{}}, nil
}

// Root is the absolute directory, shown to the model as runtime context.
func (w *Workspace) Root() string { return w.root }

// v0 §8 amendment (2026-09-27, U3): returned digests distinguish stale
// mismatches from unknown ones for each path. Tools share the workspace.
func (w *Workspace) remember(path, digest string) {
	canonical, err := filepath.EvalSymlinks(path)
	if err != nil {
		return
	}
	path = canonical
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.digests[path] == nil {
		w.digests[path] = make(map[string]bool)
	}
	w.digests[path][digest] = true
	w.seen[path] = digest
}

// v0 §8: a new conversation cannot rely on evidence it no longer contains.
func (w *Workspace) forgetSeen() {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.seen = make(map[string]string)
}

func (w *Workspace) returned(path, digest string) bool {
	canonical, err := filepath.EvalSymlinks(path)
	if err != nil {
		return false
	}
	path = canonical
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.digests[path][digest]
}

// resolve turns a model-supplied path into an absolute one, or into the
// observation explaining why it will not.
func (w *Workspace) resolve(rel string) (string, *ToolOutcome) {
	if strings.ContainsRune(rel, 0) {
		return "", failPtr("invalid_path", "path must not contain a NUL byte")
	}
	// v0 §4 amendment (2026-09-25): tolerate an empty root path and absolute
	// paths inside the workspace; the same lexical checks still apply.
	if rel == "" {
		rel = "."
	}
	if filepath.IsAbs(rel) {
		inside, err := filepath.Rel(w.root, filepath.Clean(rel))
		if err != nil || inside == ".." || strings.HasPrefix(inside, ".."+string(filepath.Separator)) {
			return "", failPtr("invalid_path", "path must be inside the workspace, "+w.root)
		}
		rel = inside
	}
	for _, part := range strings.Split(filepath.ToSlash(rel), "/") {
		switch {
		case part == "..":
			return "", failPtr("invalid_path", "path must not contain a .. component")
		case withheld(part):
			return "", failPtr("invalid_path", part+" is not readable")
		}
	}
	abs := filepath.Join(w.root, rel)
	// v0 §4 amendment (2026-09-30): verify aliases without hiding symlink leaves from writers.
	probe := abs
	for {
		_, err := os.Lstat(probe)
		if !os.IsNotExist(err) {
			break
		}
		parent := filepath.Dir(probe)
		if parent == probe {
			break
		}
		probe = parent
	}
	resolved, err := filepath.EvalSymlinks(probe)
	if err != nil {
		return "", osOutcome(err)
	}
	if !insidePath(w.root, resolved) || withheldPath(resolved) {
		return "", failPtr("invalid_path", "resolved path must be inside the workspace and not withheld")
	}
	return abs, nil
}

// withheld names the entries no file tool may see. .git is repository
// internals; .env files conventionally hold credentials, and anything a tool
// reads is sent to the provider and written to the trace (v0 §4).
func withheld(name string) bool {
	// v0 §4 amendment (2026-09-30): case aliases must not bypass withholding.
	lower := strings.ToLower(name)
	return strings.EqualFold(name, ".git") || lower == ".env" || strings.HasPrefix(lower, ".env.")
}

func insidePath(root, path string) bool {
	rel, err := filepath.Rel(root, path)
	return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

func withheldPath(path string) bool {
	for _, part := range strings.Split(filepath.ToSlash(path), "/") {
		if withheld(part) {
			return true
		}
	}
	return false
}

// Preserve aliases in observation keys even after the leaf or its parents disappear.
func missingPath(abs string) string {
	probe, suffix := abs, ""
	for {
		canonical, err := filepath.EvalSymlinks(probe)
		if err == nil {
			return filepath.Join(canonical, suffix)
		}
		parent := filepath.Dir(probe)
		if !os.IsNotExist(err) || parent == probe {
			return abs
		}
		suffix = filepath.Join(filepath.Base(probe), suffix)
		probe = parent
	}
}

// relative renders an absolute path the way the model should refer to it.
func (w *Workspace) relative(abs string) string {
	rel, err := filepath.Rel(w.root, abs)
	if err != nil {
		return abs
	}
	return filepath.ToSlash(rel)
}

// snapshot is one bounded read of a text file: its size, the digest of its
// exact bytes, and the lines derived from those same bytes. Every read tool
// reports from one snapshot, so a digest can never describe bytes other than
// the ones shown (v1 §11.3).
type snapshot struct {
	content []byte
	sha256  string
	lines   []string
}

func readSnapshot(abs string) (*snapshot, *ToolOutcome) {
	// The type is checked before opening: opening a named pipe with no writer
	// would block the whole run.
	info, err := os.Stat(abs)
	switch {
	case err != nil:
		return nil, osOutcome(err)
	case info.IsDir():
		return nil, failPtr("not_file", "path is a directory")
	case !info.Mode().IsRegular():
		return nil, failPtr("not_file", "path is not a regular file")
	}

	f, err := os.Open(abs)
	if err != nil {
		return nil, osOutcome(err)
	}
	defer f.Close()

	// One byte past the limit separates "exactly at the limit" from "over it".
	data, err := io.ReadAll(io.LimitReader(f, MaxFileBytes+1))
	switch {
	case err != nil:
		return nil, osOutcome(err)
	case len(data) > MaxFileBytes:
		return nil, failPtr("file_too_large", fmt.Sprintf("file is larger than the %d byte read limit", MaxFileBytes))
	case bytes.IndexByte(data, 0) >= 0:
		return nil, failPtr("binary_file", "file contains NUL bytes")
	case !utf8.Valid(data):
		return nil, failPtr("invalid_utf8", "file is not valid UTF-8")
	}

	sum := sha256.Sum256(data)
	return &snapshot{content: data, sha256: hex.EncodeToString(sum[:]), lines: splitLines(data)}, nil
}

// splitLines splits on LF. A trailing newline does not create a phantom line,
// and one CR before a newline is dropped for display only: the digest is always
// taken over the original bytes (v1 §11.3).
func splitLines(data []byte) []string {
	if len(data) == 0 {
		return nil
	}
	lines := strings.Split(strings.TrimSuffix(string(data), "\n"), "\n")
	for i, line := range lines {
		lines[i] = strings.TrimSuffix(line, "\r")
	}
	return lines
}

// osOutcome maps an OS error to the observation the model should read. It keeps
// the syscall text but drops the host path it was attempted on (v1 §19.1).
func osOutcome(err error) *ToolOutcome {
	switch {
	case errors.Is(err, os.ErrNotExist):
		return failPtr("not_found", "no such path in the workspace")
	case errors.Is(err, os.ErrPermission):
		return failPtr("permission_denied", "path is not readable")
	}
	var pathErr *os.PathError
	if errors.As(err, &pathErr) {
		return failPtr("io_error", pathErr.Err.Error())
	}
	return failPtr("io_error", err.Error())
}

// publish replaces a file by writing the new bytes beside it and renaming over
// it, so the target is never observed half-written.
//
// File tools hold the shared publication guard from checking through renaming.
// External processes (including exec) do not participate; publication is not synced.
func publish(abs string, content []byte, mode os.FileMode) error {
	name, err := stageFile(abs, content, mode)
	if err != nil {
		return err
	}
	// Harmless once the rename has consumed the temporary file.
	defer os.Remove(name)
	return os.Rename(name, abs)
}

// v0 §8 amendment (2026-09-27): linking a staged file makes create exclusive;
// rename would overwrite an unguarded file that appeared during this call.
func publishNew(abs string, content []byte) error {
	name, err := stageFile(abs, content, 0o644)
	if err != nil {
		return err
	}
	defer os.Remove(name)
	return os.Link(name, abs)
}

func stageFile(abs string, content []byte, mode os.FileMode) (string, error) {
	temp, err := os.CreateTemp(filepath.Dir(abs), ".reagent-*")
	if err != nil {
		return "", err
	}
	name := temp.Name()
	if _, err := temp.Write(content); err != nil {
		temp.Close()
		os.Remove(name)
		return "", err
	}
	if err := temp.Close(); err != nil {
		os.Remove(name)
		return "", err
	}
	if err := os.Chmod(name, mode); err != nil {
		os.Remove(name)
		return "", err
	}
	return name, nil
}

// digestOf is the same SHA-256 a snapshot reports, for bytes about to be
// written rather than bytes just read.
func digestOf(content string) string {
	sum := sha256.Sum256([]byte(content))
	return hex.EncodeToString(sum[:])
}
