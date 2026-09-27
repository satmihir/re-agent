package reagent

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestWriteFile_CreatesAndOverwrites(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"sub/old.txt": "before\n"})
	create := runTool(t, NewWriteFileTool(ws), `{"path":"sub/new.txt","content":"hello\n"}`)
	var made writeFileResult
	data(t, create, &made)
	if create.Effect != EffectApplied || made.Operation != "create" || made.Path != "sub/new.txt" ||
		made.BeforeSHA256 != nil || made.AfterSHA256 != digestOfFile(t, ws, "sub/new.txt") ||
		made.SizeBytes != len("hello\n") || fileContent(t, ws, "sub/new.txt") != "hello\n" {
		t.Fatalf("create: %+v %+v", create, made)
	}
	if info, err := os.Stat(filepath.Join(ws.Root(), "sub/new.txt")); err != nil || info.Mode().Perm() != 0o644 {
		t.Fatalf("created file mode: %v, %v", info, err)
	}

	path := filepath.Join(ws.Root(), "sub/old.txt")
	if err := os.Chmod(path, 0o755); err != nil {
		t.Fatal(err)
	}
	before := readDigest(t, ws, "sub/old.txt")
	overwrite := runTool(t, NewWriteFileTool(ws), `{"path":"sub/old.txt","content":"after\n","expected_sha256":"`+before+`"}`)
	var replaced writeFileResult
	data(t, overwrite, &replaced)
	if overwrite.Effect != EffectApplied || replaced.Operation != "overwrite" || replaced.Path != "sub/old.txt" ||
		replaced.BeforeSHA256 == nil || *replaced.BeforeSHA256 != before ||
		replaced.AfterSHA256 != digestOfFile(t, ws, "sub/old.txt") || replaced.SizeBytes != len("after\n") ||
		fileContent(t, ws, "sub/old.txt") != "after\n" {
		t.Fatalf("overwrite: %+v %+v", overwrite, replaced)
	}
	if info, err := os.Stat(path); err != nil || info.Mode().Perm() != 0o755 {
		t.Fatalf("overwritten file mode: %v, %v", info, err)
	}
}

func TestFileWriters_ReturnedDigestsWorkForNextWrite(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"old.txt": "old"})
	var made writeFileResult
	data(t, runTool(t, NewWriteFileTool(ws), `{"path":"new.txt","content":"first"}`), &made)
	var updated writeFileResult
	data(t, runTool(t, NewWriteFileTool(ws), `{"path":"new.txt","content":"second","expected_sha256":"`+made.AfterSHA256+`"}`), &updated)
	if updated.BeforeSHA256 == nil || *updated.BeforeSHA256 != made.AfterSHA256 {
		t.Fatalf("create %+v, overwrite %+v", made, updated)
	}
	oldDigest := readDigest(t, ws, "old.txt")
	var replaced writeFileResult
	data(t, runTool(t, NewWriteFileTool(ws), `{"path":"old.txt","content":"changed","expected_sha256":"`+
		oldDigest+`"}`), &replaced)
	var deleted deleteFileResult
	data(t, runTool(t, NewDeleteFileTool(ws), `{"path":"old.txt","expected_sha256":"`+replaced.AfterSHA256+`"}`), &deleted)
	if deleted.BeforeSHA256 != replaced.AfterSHA256 || fileContent(t, ws, "new.txt") != "second" {
		t.Fatalf("overwrite %+v, delete %+v", replaced, deleted)
	}
}

func TestFileWriters_CorrectDigestWorksWithoutRead(t *testing.T) {
	for _, tc := range []struct {
		name string
		tool func(*Workspace) Tool
		args func(string) string
	}{
		{"write", NewWriteFileTool, func(d string) string { return `{"path":"old.txt","content":"new","expected_sha256":"` + d + `"}` }},
		{"delete", NewDeleteFileTool, func(d string) string { return `{"path":"old.txt","expected_sha256":"` + d + `"}` }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ws := testWorkspace(t, map[string]string{"old.txt": "old"})
			digest := digestOfFile(t, ws, "old.txt")
			outcome := runTool(t, tc.tool(ws), tc.args(digest))
			if !outcome.OK || outcome.Effect != EffectApplied {
				t.Fatalf("got %+v", outcome)
			}
		})
	}
}

func TestFileWriters_StaleAfterExternalChange(t *testing.T) {
	for _, tc := range []struct {
		name string
		tool func(*Workspace) Tool
		args func(string) string
	}{
		{"write", NewWriteFileTool, func(d string) string { return `{"path":"old.txt","content":"new","expected_sha256":"` + d + `"}` }},
		{"delete", NewDeleteFileTool, func(d string) string { return `{"path":"old.txt","expected_sha256":"` + d + `"}` }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ws := testWorkspace(t, map[string]string{"old.txt": "old"})
			stale := readDigest(t, ws, "old.txt")
			if err := os.WriteFile(filepath.Join(ws.Root(), "old.txt"), []byte("changed"), 0o600); err != nil {
				t.Fatal(err)
			}
			outcome := runTool(t, tc.tool(ws), tc.args(stale))
			if outcome.Code != "stale_file" || outcome.Effect != EffectNone || fileContent(t, ws, "old.txt") != "changed" {
				t.Fatalf("got %+v", outcome)
			}
		})
	}
}

func TestWriteFile_RefusedChangesLeaveWorkspaceAlone(t *testing.T) {
	badDigest := strings.Repeat("a", 64)
	cases := []struct {
		name, args, code, message string
	}{
		{"unknown overwrite", `{"path":"old.txt","content":"new","expected_sha256":"` + badDigest + `"}`, "unknown_digest", ""},
		{"existing no digest", `{"path":"old.txt","content":"new"}`, "invalid_arguments", "the file exists; read it and pass its sha256 to overwrite it"},
		{"missing with digest", `{"path":"absent","content":"new","expected_sha256":"` + badDigest + `"}`, "not_found", ""},
		{"missing parent", `{"path":"missing/new.txt","content":"new"}`, "not_found", ""},
		{"directory", `{"path":"sub","content":"new"}`, "invalid_arguments", ""},
		{"withheld", `{"path":".env","content":"new"}`, "invalid_path", ""},
		{"outside", `{"path":"../outside","content":"new"}`, "invalid_path", ""},
		{"too large", `{"path":"big.txt","content":"` + strings.Repeat("x", MaxFileBytes+1) + `"}`, "file_too_large", ""},
		{"invalid utf8", "{\"path\":\"bad.txt\",\"content\":\"" + string([]byte{0xff}) + "\"}", "invalid_utf8", ""},
		{"nul", `{"path":"nul.txt","content":"a\u0000b"}`, "binary_file", ""},
		{"null digest", `{"path":"new.txt","content":"x","expected_sha256":null}`, "invalid_arguments", "expected_sha256 must be the 64-character digest read_file returned"},
		{"short digest", `{"path":"old.txt","content":"x","expected_sha256":"` + strings.Repeat("a", 63) + `"}`, "invalid_arguments", "expected_sha256 must be the 64-character digest read_file returned"},
		{"uppercase digest", `{"path":"old.txt","content":"x","expected_sha256":"` + strings.Repeat("A", 64) + `"}`, "invalid_arguments", "expected_sha256 must be the 64-character digest read_file returned"},
		{"null content", `{"path":"new.txt","content":null}`, "invalid_arguments", ""},
		{"missing content", `{"path":"new.txt"}`, "invalid_arguments", ""},
		{"unknown field", `{"path":"new.txt","content":"x","force":true}`, "invalid_arguments", ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ws := testWorkspace(t, map[string]string{"old.txt": "original", "sub/keep": "keep"})
			outcome := runTool(t, NewWriteFileTool(ws), tc.args)
			if outcome.OK || outcome.Code != tc.code || outcome.Effect != EffectNone {
				t.Fatalf("got %+v, want %s", outcome, tc.code)
			}
			if tc.message != "" && outcome.Message != tc.message {
				t.Fatalf("got message %q, want %q", outcome.Message, tc.message)
			}
			if got := fileContent(t, ws, "old.txt"); got != "original" {
				t.Fatalf("original changed: %q", got)
			}
			if _, err := os.Lstat(filepath.Join(ws.Root(), "new.txt")); !os.IsNotExist(err) {
				t.Fatalf("new file unexpectedly exists: %v", err)
			}
		})
	}
}

func TestWriteFile_ExclusiveCreateDoesNotReplaceAnArrival(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"already.txt": "keep"})
	path := filepath.Join(ws.Root(), "already.txt")
	if err := publishNew(path, []byte("replace")); !errors.Is(err, os.ErrExist) {
		t.Fatalf("got %v, want file exists", err)
	}
	if fileContent(t, ws, "already.txt") != "keep" {
		t.Fatal("create replaced an existing file")
	}
	entries, err := os.ReadDir(ws.Root())
	if err != nil || len(entries) != 1 {
		t.Fatalf("staged file left behind: %v, %v", entries, err)
	}
}

func TestDeleteFile_DeletesOnlyMatchingRegularFile(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"gone.txt": "remove\n", "keep.txt": "stay"})
	before := readDigest(t, ws, "gone.txt")
	outcome := runTool(t, NewDeleteFileTool(ws), `{"path":"gone.txt","expected_sha256":"`+before+`"}`)
	var got deleteFileResult
	data(t, outcome, &got)
	if outcome.Effect != EffectApplied || got.Operation != "delete" || got.Path != "gone.txt" || got.BeforeSHA256 != before {
		t.Fatalf("got %+v %+v", outcome, got)
	}
	if _, err := os.Lstat(filepath.Join(ws.Root(), "gone.txt")); !os.IsNotExist(err) {
		t.Fatalf("file not deleted: %v", err)
	}
	if fileContent(t, ws, "keep.txt") != "stay" {
		t.Fatal("another file changed")
	}
}

func TestDeleteFile_Refusals(t *testing.T) {
	cases := []struct{ name, args, code string }{
		{"unknown", `{"path":"old.txt","expected_sha256":"` + strings.Repeat("a", 64) + `"}`, "unknown_digest"},
		{"directory", `{"path":"sub","expected_sha256":"` + strings.Repeat("a", 64) + `"}`, "invalid_arguments"},
		{"missing", `{"path":"absent","expected_sha256":"` + strings.Repeat("a", 64) + `"}`, "not_found"},
		{"missing digest", `{"path":"old.txt"}`, "invalid_arguments"},
		{"malformed digest", `{"path":"old.txt","expected_sha256":"short"}`, "invalid_arguments"},
		{"withheld", `{"path":".env","expected_sha256":"` + strings.Repeat("a", 64) + `"}`, "invalid_path"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ws := testWorkspace(t, map[string]string{"old.txt": "original", "sub/keep": "keep"})
			outcome := runTool(t, NewDeleteFileTool(ws), tc.args)
			if outcome.OK || outcome.Code != tc.code || outcome.Effect != EffectNone {
				t.Fatalf("got %+v, want %s", outcome, tc.code)
			}
			if fileContent(t, ws, "old.txt") != "original" {
				t.Fatal("refused deletion removed the file")
			}
		})
	}
}

func TestDeleteFile_MalformedDigestMessage(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"old.txt": "old"})
	outcome := runTool(t, NewDeleteFileTool(ws), `{"path":"old.txt","expected_sha256":"`+strings.Repeat("a", 63)+`"}`)
	if outcome.OK || outcome.Code != "invalid_arguments" || outcome.Effect != EffectNone ||
		outcome.Message != "expected_sha256 must be the 64-character digest read_file returned" {
		t.Fatalf("got %+v", outcome)
	}
}

func TestFileWriters_RejectSymlinkTargets(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"real.txt": "leave alone"})
	if err := os.Symlink(filepath.Join(ws.Root(), "real.txt"), filepath.Join(ws.Root(), "link.txt")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	before := digestOfFile(t, ws, "real.txt")
	for _, tc := range []struct {
		name string
		tool Tool
		args string
	}{
		{"write", NewWriteFileTool(ws), `{"path":"link.txt","content":"changed","expected_sha256":"` + before + `"}`},
		{"delete", NewDeleteFileTool(ws), `{"path":"link.txt","expected_sha256":"` + before + `"}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			outcome := runTool(t, tc.tool, tc.args)
			if outcome.OK || outcome.Code != "symlink_target" || outcome.Effect != EffectNone {
				t.Fatalf("got %+v", outcome)
			}
			if fileContent(t, ws, "real.txt") != "leave alone" {
				t.Fatal("symlink target changed")
			}
			if info, err := os.Lstat(filepath.Join(ws.Root(), "link.txt")); err != nil || info.Mode()&os.ModeSymlink == 0 {
				t.Fatalf("link changed: %v, %v", info, err)
			}
		})
	}
}

func TestRegistry_ReadOnlyWithholdsFileWriters(t *testing.T) {
	ws := testWorkspace(t, nil)
	tools := []Tool{NewReadFileTool(ws), NewEditFileTool(ws), NewWriteFileTool(ws), NewDeleteFileTool(ws)}
	readOnly, err := NewRegistry(Mode{ReadOnly: true}, tools...)
	if err != nil {
		t.Fatal(err)
	}
	if len(readOnly.Specs()) != 1 || readOnly.Specs()[0].Name != "read_file" {
		t.Fatalf("writers declared in read-only mode: %+v", readOnly.Specs())
	}
	for _, name := range []string{"edit_file", "write_file", "delete_file"} {
		if _, ok := readOnly.Lookup(name); ok || !readOnly.known(name) {
			t.Fatalf("%s should be withheld but known", name)
		}
	}
	writable, err := NewRegistry(Mode{}, tools...)
	if err != nil {
		t.Fatal(err)
	}
	if len(writable.Specs()) != len(tools) {
		t.Fatalf("writers missing: %+v", writable.Specs())
	}
}

func TestLoop_FileWriterEffectsOmitContentAndDigest(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"old.txt": "old"})
	before := readDigest(t, ws, "old.txt")
	registry, err := NewRegistry(Mode{}, NewWriteFileTool(ws), NewDeleteFileTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Model: "test", Registry: registry, WorkspacePath: ws.Root(), MaxSteps: 10, MaxToolCalls: 10}
	_, result := runScript(t, cfg,
		turn(callBlock("create", "write_file", `{"path":"new.txt","content":"private"}`),
			callBlock("delete", "delete_file", `{"path":"old.txt","expected_sha256":"`+before+`"}`)),
		turn(textBlock("done")))
	if result.Status != StatusCompleted || len(result.Effects) != 2 {
		t.Fatalf("got status %s effects %+v", result.Status, result.Effects)
	}
	for _, effect := range result.Effects {
		if effect.Effect != EffectApplied || strings.Contains(effect.Summary, "private") ||
			strings.Contains(effect.Summary, "expected_sha256") || !strings.Contains(effect.Summary, "path=") {
			t.Fatalf("bad file effect: %+v", effect)
		}
	}
}

func TestLoop_ReadOnlyRefusesFileWriters(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"old.txt": "old"})
	registry, err := NewRegistry(Mode{ReadOnly: true}, NewWriteFileTool(ws), NewDeleteFileTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Model: "test", Registry: registry, WorkspacePath: ws.Root(), MaxSteps: 10, MaxToolCalls: 10}
	run, result := runScript(t, cfg,
		turn(callBlock("create", "write_file", `{"path":"new.txt","content":"private"}`),
			callBlock("delete", "delete_file", `{"path":"old.txt","expected_sha256":"x"}`)),
		turn(textBlock("cannot write")))
	if result.Status != StatusCompleted || len(result.Effects) != 0 {
		t.Fatalf("got status %s effects %+v", result.Status, result.Effects)
	}
	for _, got := range results(run) {
		if got.Outcome.Code != "permission_denied" || got.Outcome.Effect != EffectNone {
			t.Fatalf("withheld tool ran: %+v", got)
		}
	}
	if fileContent(t, ws, "old.txt") != "old" {
		t.Fatal("read-only deleted the file")
	}
	if _, err := os.Lstat(filepath.Join(ws.Root(), "new.txt")); !os.IsNotExist(err) {
		t.Fatalf("read-only created a file: %v", err)
	}
}
