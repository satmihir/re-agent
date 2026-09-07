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
	"unicode/utf8"
)

// Workspace is the directory the read tools may see.
//
// Its path checks are lexical (v0 §4). They stop the obvious escapes, but they
// are not a sandbox and do not defend against filesystem aliases; v1 §11.1
// restores rooted access.
type Workspace struct {
	root string
}

// OpenWorkspace resolves the directory once, at startup.
func OpenWorkspace(path string) (*Workspace, error) {
	root, err := filepath.Abs(path)
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
	return &Workspace{root: root}, nil
}

// Root is the absolute directory, shown to the model as runtime context.
func (w *Workspace) Root() string { return w.root }

// resolve turns a model-supplied relative path into an absolute one, or into
// the observation explaining why it will not.
func (w *Workspace) resolve(rel string) (string, *ToolOutcome) {
	switch {
	case rel == "":
		return "", failPtr("invalid_path", "path must not be empty")
	case strings.ContainsRune(rel, 0):
		return "", failPtr("invalid_path", "path must not contain a NUL byte")
	case filepath.IsAbs(rel):
		return "", failPtr("invalid_path", "path must be relative to the workspace")
	}
	for _, part := range strings.Split(filepath.ToSlash(rel), "/") {
		switch part {
		case "..":
			return "", failPtr("invalid_path", "path must not contain a .. component")
		case ".git":
			return "", failPtr("invalid_path", ".git is not readable")
		}
	}
	return filepath.Join(w.root, rel), nil
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
// v0 stops there: it does not sync, re-check the digest immediately before the
// rename, or reconcile an interrupted publication (v1 §13.4 restores those).
func publish(abs string, content []byte, mode os.FileMode) error {
	temp, err := os.CreateTemp(filepath.Dir(abs), ".reagent-*")
	if err != nil {
		return err
	}
	name := temp.Name()
	// Harmless once the rename has consumed the temporary file.
	defer os.Remove(name)

	if _, err := temp.Write(content); err != nil {
		temp.Close()
		return err
	}
	if err := temp.Close(); err != nil {
		return err
	}
	if err := os.Chmod(name, mode); err != nil {
		return err
	}
	return os.Rename(name, abs)
}

// digestOf is the same SHA-256 a snapshot reports, for bytes about to be
// written rather than bytes just read.
func digestOf(content string) string {
	sum := sha256.Sum256([]byte(content))
	return hex.EncodeToString(sum[:])
}
