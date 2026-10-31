package reagent

import (
	"errors"
	"os"
	"path/filepath"
	"syscall"
	"testing"
)

func TestExecProcessSnapshot_VanishedPIDReadErrors(t *testing.T) {
	for _, tc := range []struct {
		name     string
		err      error
		vanished bool
	}{
		{"not found", syscall.ENOENT, true},
		{"no such process", syscall.ESRCH, true},
		{"permission denied", syscall.EACCES, false},
		{"operation not permitted", syscall.EPERM, false},
		{"io error", syscall.EIO, false},
		{"interrupted", syscall.EINTR, false},
		{"not a directory", syscall.ENOTDIR, false},
		{"is a directory", syscall.EISDIR, false},
		{"other", errors.New("stat read failed"), false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root := t.TempDir()
			for _, pid := range []string{"123", "456"} {
				if err := os.Mkdir(filepath.Join(root, pid), 0700); err != nil {
					t.Fatal(err)
				}
			}
			readErr := &os.PathError{Op: "read", Path: filepath.Join(root, "123", "stat"), Err: tc.err}
			processes, err := execProcessSnapshotFrom(root, func(path string) ([]byte, error) {
				if path == readErr.Path {
					return nil, readErr
				}
				if path != filepath.Join(root, "456", "stat") {
					t.Fatalf("unexpected stat path %q", path)
				}
				return []byte("456 (command with ) spaces) S 1 456"), nil
			})
			if !tc.vanished {
				if err != readErr {
					t.Fatalf("got %v, want original read error %v", err, readErr)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			want := execProcess{pid: 456, ppid: 1, pgid: 456, state: "S"}
			if len(processes) != 1 || processes[0] != want {
				t.Fatalf("got %+v, want [%+v]", processes, want)
			}
		})
	}
}

func TestExecProcessSnapshot_RealReadsPreserveDiscoveryErrors(t *testing.T) {
	root := t.TempDir()
	if _, err := execProcessSnapshotFrom(filepath.Join(root, "missing"), os.ReadFile); !os.IsNotExist(err) {
		t.Fatalf("missing root: %v", err)
	}
	pid := filepath.Join(root, "123")
	if err := os.Mkdir(pid, 0700); err != nil {
		t.Fatal(err)
	}
	if processes, err := execProcessSnapshotFrom(root, os.ReadFile); err != nil || len(processes) != 0 {
		t.Fatalf("missing stat: %+v, %v", processes, err)
	}
	stat := filepath.Join(pid, "stat")
	if err := os.WriteFile(stat, []byte("malformed"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := execProcessSnapshotFrom(root, os.ReadFile); err == nil {
		t.Fatal("malformed stat accepted")
	}
	if err := os.Remove(stat); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(stat, 0700); err != nil {
		t.Fatal(err)
	}
	if _, err := execProcessSnapshotFrom(root, os.ReadFile); !errors.Is(err, syscall.EISDIR) {
		t.Fatalf("directory stat: %v", err)
	}
}
