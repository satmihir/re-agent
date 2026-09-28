package reagent

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

func projectPreview(root string) (int, string, string) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run", "--workspace", root, "--show-context", "task"}, strings.NewReader(""), &stdout, &stderr)
	return code, stdout.String(), stderr.String()
}

func TestProjectInstructions_SymlinkOutsideIsSkipped(t *testing.T) {
	dir := t.TempDir()
	root := filepath.Join(dir, "workspace")
	if err := os.Mkdir(root, 0o700); err != nil {
		t.Fatal(err)
	}
	secret := filepath.Join(dir, "secret")
	if err := os.WriteFile(secret, []byte("AWS_SECRET=not-for-model"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(secret, filepath.Join(root, "AGENTS.md")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	code, body, warning := projectPreview(root)
	if code != exitOK || strings.Contains(body, "not-for-model") || strings.Contains(body, "Project instructions (AGENTS.md)") || !strings.Contains(warning, "symlink points outside the workspace") || strings.Count(warning, "\n") != 1 {
		t.Fatalf("exit %d, leaked=%t, warning %q", code, strings.Contains(body, "not-for-model"), warning)
	}
}

func TestProjectInstructions_SymlinkToWithheldIsSkipped(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, ".env"), []byte("API_KEY=not-for-model"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(".env", filepath.Join(root, "AGENTS.md")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	code, body, warning := projectPreview(root)
	if code != exitOK || strings.Contains(body, "not-for-model") || strings.Contains(body, "Project instructions (AGENTS.md)") || !strings.Contains(warning, "symlink target is withheld") || strings.Count(warning, "\n") != 1 {
		t.Fatalf("exit %d, leaked=%t, warning %q", code, strings.Contains(body, "not-for-model"), warning)
	}
}

func TestProjectInstructions_SymlinkInsideIsLoaded(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "CLAUDE.md"), []byte("Allowed project rule\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("CLAUDE.md", filepath.Join(root, "AGENTS.md")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	code, body, warning := projectPreview(root)
	if code != exitOK || !strings.Contains(body, "Allowed project rule") || !strings.Contains(body, "Project instructions (AGENTS.md)") || warning != "" {
		t.Fatalf("exit %d, loaded=%t, warning %q", code, strings.Contains(body, "Allowed project rule"), warning)
	}
}

func TestProjectInstructions_SymlinkInsideWithWorkspaceAliasIsLoaded(t *testing.T) {
	parent := t.TempDir()
	root := filepath.Join(parent, "repo")
	if err := os.Mkdir(root, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "CLAUDE.md"), []byte("From aliased workspace\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("CLAUDE.md", filepath.Join(root, "AGENTS.md")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	alias := filepath.Join(parent, "alias")
	if err := os.Symlink(root, alias); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	code, body, warning := projectPreview(alias)
	if code != exitOK || !strings.Contains(body, "From aliased workspace") || warning != "" {
		t.Fatalf("exit %d, loaded=%t, warning %q", code, strings.Contains(body, "From aliased workspace"), warning)
	}
}

func TestProjectInstructions_NamedPipeIsSkippedWithoutBlocking(t *testing.T) {
	root := t.TempDir()
	if err := syscall.Mkfifo(filepath.Join(root, "AGENTS.md"), 0o600); err != nil {
		t.Skipf("named pipes unavailable: %v", err)
	}
	done := make(chan struct {
		code          int
		body, warning string
	}, 1)
	go func() {
		code, body, warning := projectPreview(root)
		done <- struct {
			code          int
			body, warning string
		}{code, body, warning}
	}()
	select {
	case result := <-done:
		if result.code != exitOK || strings.Contains(result.body, "Project instructions (AGENTS.md)") || !strings.Contains(result.warning, "not a regular file") {
			t.Fatalf("exit %d, warning %q", result.code, result.warning)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("loading AGENTS.md blocked on a named pipe")
	}
}

func TestProjectInstructions_DirectoryIsSkipped(t *testing.T) {
	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, "AGENTS.md"), 0o700); err != nil {
		t.Fatal(err)
	}
	code, body, warning := projectPreview(root)
	if code != exitOK || strings.Contains(body, "Project instructions (AGENTS.md)") || !strings.Contains(warning, "not a regular file") || strings.Count(warning, "\n") != 1 {
		t.Fatalf("exit %d, warning %q", code, warning)
	}
}
