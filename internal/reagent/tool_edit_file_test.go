package reagent

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const source = "package main\n\nconst timeout = 0\n"

func digestOfFile(t *testing.T, ws *Workspace, name string) string {
	t.Helper()
	content, err := os.ReadFile(filepath.Join(ws.Root(), name))
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(content)
	return hex.EncodeToString(sum[:])
}

func readDigest(t *testing.T, ws *Workspace, name string) string {
	t.Helper()
	args, err := json.Marshal(map[string]string{"path": name})
	if err != nil {
		t.Fatal(err)
	}
	var result readFileResult
	data(t, runTool(t, NewReadFileTool(ws), string(args)), &result)
	return result.SHA256
}

func fileContent(t *testing.T, ws *Workspace, name string) string {
	t.Helper()
	content, err := os.ReadFile(filepath.Join(ws.Root(), name))
	if err != nil {
		t.Fatal(err)
	}
	return string(content)
}

func TestEditFile_ReplacesTheUniqueMatch(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"main.go": source})
	before := readDigest(t, ws, "main.go")

	outcome := runTool(t, NewEditFileTool(ws), `{"path":"main.go","expected_sha256":"`+before+
		`","old_text":"const timeout = 0","new_text":"const timeout = 30"}`)

	var got editFileResult
	data(t, outcome, &got)
	if !got.Changed || got.Operation != "update" || got.Path != "main.go" {
		t.Fatalf("got %+v", got)
	}
	if outcome.Effect != EffectApplied {
		t.Fatalf("got effect %s, want applied", outcome.Effect)
	}

	after := fileContent(t, ws, "main.go")
	if after != "package main\n\nconst timeout = 30\n" {
		t.Fatalf("got %q", after)
	}
	// The reported digests describe the bytes before and after, on disk.
	if got.BeforeSHA256 != before || got.AfterSHA256 != digestOfFile(t, ws, "main.go") {
		t.Fatalf("got %+v", got)
	}
	if got.SizeBytes != len(after) {
		t.Fatalf("got size %d, want %d", got.SizeBytes, len(after))
	}
}

func TestEditFile_AfterDigestAllowsNextEdit(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"main.go": source})
	before := readDigest(t, ws, "main.go")
	var first editFileResult
	data(t, runTool(t, NewEditFileTool(ws), `{"path":"main.go","expected_sha256":"`+before+
		`","old_text":"timeout = 0","new_text":"timeout = 30"}`), &first)
	var second editFileResult
	data(t, runTool(t, NewEditFileTool(ws), `{"path":"main.go","expected_sha256":"`+first.AfterSHA256+
		`","old_text":"timeout = 30","new_text":"timeout = 60"}`), &second)
	if second.BeforeSHA256 != first.AfterSHA256 || fileContent(t, ws, "main.go") != strings.Replace(source, "timeout = 0", "timeout = 60", 1) {
		t.Fatalf("first %+v, second %+v", first, second)
	}
}

func TestEditFile_MalformedDigest(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"main.go": source})
	for name, digest := range map[string]string{
		"63 characters": strings.Repeat("a", 63),
		"uppercase":     strings.ToUpper(digestOfFile(t, ws, "main.go")),
	} {
		t.Run(name, func(t *testing.T) {
			outcome := runTool(t, NewEditFileTool(ws), `{"path":"main.go","expected_sha256":"`+digest+
				`","old_text":"timeout","new_text":"delay"}`)
			if outcome.OK || outcome.Code != "invalid_arguments" || outcome.Effect != EffectNone ||
				outcome.Message != "expected_sha256 must be the 64-character digest read_file returned" {
				t.Fatalf("got %+v", outcome)
			}
		})
	}
}

func TestEditFile_UnknownDigestIsNotStale(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"main.go": source, "other.go": source})
	current := readDigest(t, ws, "main.go")
	unknown := strings.Repeat("a", 64)
	outcome := runTool(t, NewEditFileTool(ws), `{"path":"main.go","expected_sha256":"`+unknown+
		`","old_text":"timeout","new_text":"delay"}`)
	want := "this is not a digest read_file returned for this file; its current digest is " + current
	if outcome.OK || outcome.Code != "unknown_digest" || outcome.Effect != EffectNone || outcome.Message != want {
		t.Fatalf("got %+v, want %q", outcome, want)
	}
	// The same bytes in another file do not authorize editing that file.
	outcome = runTool(t, NewEditFileTool(ws), `{"path":"other.go","expected_sha256":"`+current+
		`","old_text":"timeout","new_text":"delay"}`)
	if outcome.Code != "unknown_digest" || outcome.Effect != EffectNone || fileContent(t, ws, "other.go") != source {
		t.Fatalf("got %+v", outcome)
	}
	if fileContent(t, ws, "main.go") != source {
		t.Fatal("a refused edit changed the file")
	}
}

func TestEditFile_StaleAfterAnotherWrite(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"main.go": source})
	stale := readDigest(t, ws, "main.go")
	changed := strings.Replace(source, "timeout = 0", "timeout = 1", 1)
	if err := os.WriteFile(filepath.Join(ws.Root(), "main.go"), []byte(changed), 0o600); err != nil {
		t.Fatal(err)
	}
	outcome := runTool(t, NewEditFileTool(ws), `{"path":"main.go","expected_sha256":"`+stale+
		`","old_text":"timeout","new_text":"delay"}`)
	want := "the file has changed since it was read; its current digest is " + digestOfFile(t, ws, "main.go")
	if outcome.OK || outcome.Code != "stale_file" || outcome.Effect != EffectNone || outcome.Message != want {
		t.Fatalf("got %+v, want %q", outcome, want)
	}
	if fileContent(t, ws, "main.go") != changed {
		t.Fatal("a refused edit changed the file")
	}
}

func TestEditFile_NoOpEditRejected(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"main.go": source})
	before := readDigest(t, ws, "main.go")
	path := filepath.Join(ws.Root(), "main.go")
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	outcome := runTool(t, NewEditFileTool(ws), `{"path":"main.go","expected_sha256":"`+before+
		`","old_text":"timeout","new_text":"timeout"}`)
	if outcome.OK || outcome.Code != "invalid_arguments" || outcome.Effect != EffectNone ||
		outcome.Message != "old_text and new_text are the same, so the edit changes nothing" {
		t.Fatalf("got %+v", outcome)
	}
	if digestOfFile(t, ws, "main.go") != before {
		t.Fatal("a refused edit changed the file")
	}
	after, err := os.Stat(path)
	if err != nil || !after.ModTime().Equal(info.ModTime()) {
		t.Fatalf("a refused edit rewrote the file: %v, %v", after, err)
	}
}

func TestEditFile_RejectedEditsLeaveTheFileAlone(t *testing.T) {
	ws := testWorkspace(t, map[string]string{
		"main.go": source,
		"twice":   "repeat\nrepeat\n",
		"big":     strings.Repeat("x\n", (MaxFileBytes-100)/2) + "MARKER",
	})
	digest := func(name string) string { return readDigest(t, ws, name) }

	cases := map[string]struct{ args, code string }{
		"missing text": {`{"path":"main.go","expected_sha256":"` + digest("main.go") +
			`","old_text":"absent","new_text":"x"}`, "edit_not_found"},
		"ambiguous text": {`{"path":"twice","expected_sha256":"` + digest("twice") +
			`","old_text":"repeat","new_text":"x"}`, "ambiguous_edit"},
		"missing file": {`{"path":"absent.go","expected_sha256":"` + strings.Repeat("b", 64) +
			`","old_text":"a","new_text":"b"}`, "not_found"},
		"empty old text": {`{"path":"main.go","expected_sha256":"` + digest("main.go") +
			`","old_text":"","new_text":"x"}`, "invalid_arguments"},
		"malformed digest": {`{"path":"main.go","expected_sha256":"short","old_text":"a","new_text":"b"}`,
			"invalid_arguments"},
		"uppercase digest": {`{"path":"main.go","expected_sha256":"` + strings.ToUpper(digest("main.go")) +
			`","old_text":"a","new_text":"b"}`, "invalid_arguments"},
		"escaping path": {`{"path":"../outside","expected_sha256":"` + strings.Repeat("c", 64) +
			`","old_text":"a","new_text":"b"}`, "invalid_path"},
		"unknown field": {`{"path":"main.go","expected_sha256":"` + digest("main.go") +
			`","old_text":"a","new_text":"b","mode":"force"}`, "invalid_arguments"},
		"result too large": {`{"path":"big","expected_sha256":"` + digest("big") +
			`","old_text":"MARKER","new_text":"` + strings.Repeat("y", 200) + `"}`, "file_too_large"},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			outcome := runTool(t, NewEditFileTool(ws), c.args)
			if outcome.OK || outcome.Code != c.code {
				t.Fatalf("got %s (%s), want %s", outcome.Code, outcome.Message, c.code)
			}
			if outcome.Effect != EffectNone {
				t.Fatalf("a rejected edit reported effect %s", outcome.Effect)
			}
		})
	}
	if fileContent(t, ws, "main.go") != source {
		t.Fatal("a rejected edit changed the file")
	}
}

// Overlapping matches are ambiguous too: replacing one of them would silently
// pick a position the model did not choose.
func TestEditFile_OverlappingMatchesAreAmbiguous(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "aaa"})
	outcome := runTool(t, NewEditFileTool(ws), `{"path":"a.txt","expected_sha256":"`+
		readDigest(t, ws, "a.txt")+`","old_text":"aa","new_text":"b"}`)

	if outcome.Code != "ambiguous_edit" {
		t.Fatalf("got %s (%s)", outcome.Code, outcome.Message)
	}
}

func TestEditFile_EmptyNewTextDeletesTheMatch(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "keep\nremove\n"})
	outcome := runTool(t, NewEditFileTool(ws), `{"path":"a.txt","expected_sha256":"`+
		readDigest(t, ws, "a.txt")+`","old_text":"remove\n","new_text":""}`)

	var got editFileResult
	data(t, outcome, &got)
	if !got.Changed || fileContent(t, ws, "a.txt") != "keep\n" {
		t.Fatalf("got %q", fileContent(t, ws, "a.txt"))
	}
}

func TestEditFile_PreservesPermissionBits(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"script.sh": "echo old\n"})
	path := filepath.Join(ws.Root(), "script.sh")
	if err := os.Chmod(path, 0o755); err != nil {
		t.Fatal(err)
	}

	runTool(t, NewEditFileTool(ws), `{"path":"script.sh","expected_sha256":"`+
		readDigest(t, ws, "script.sh")+`","old_text":"old","new_text":"new"}`)

	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o755 {
		t.Fatalf("got mode %v, want 0755", info.Mode().Perm())
	}
}

func TestEditFile_RefusesASymlinkTarget(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"real.txt": "hello\n"})
	if err := os.Symlink(filepath.Join(ws.Root(), "real.txt"), filepath.Join(ws.Root(), "link.txt")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	outcome := runTool(t, NewEditFileTool(ws), `{"path":"link.txt","expected_sha256":"`+
		digestOfFile(t, ws, "real.txt")+`","old_text":"hello","new_text":"goodbye"}`)

	if outcome.Code != "symlink_target" {
		t.Fatalf("got %s (%s)", outcome.Code, outcome.Message)
	}
	if fileContent(t, ws, "real.txt") != "hello\n" {
		t.Fatal("the symlink's target was edited")
	}
}

func TestEditFile_RefusesAnUnreadableTarget(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"binary": "before\x00after"})
	outcome := runTool(t, NewEditFileTool(ws), `{"path":"binary","expected_sha256":"`+
		digestOfFile(t, ws, "binary")+`","old_text":"before","new_text":"x"}`)

	if outcome.Code != "binary_file" || outcome.Effect != EffectNone {
		t.Fatalf("got %+v", outcome)
	}
}

// A NUL byte is the one way a replacement can stop the file being text.
func TestEditFile_RefusesAResultWithNulBytes(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "hello\n"})
	outcome := runTool(t, NewEditFileTool(ws), `{"path":"a.txt","expected_sha256":"`+
		readDigest(t, ws, "a.txt")+`","old_text":"hello","new_text":"he\u0000llo"}`)

	if outcome.Code != "binary_file" || outcome.Effect != EffectNone {
		t.Fatalf("got %+v", outcome)
	}
	if fileContent(t, ws, "a.txt") != "hello\n" {
		t.Fatal("the file was changed")
	}
}

// A publication that cannot start leaves the original in place, so the outcome
// reports no effect rather than an uncertain one.
func TestEditFile_FailedPublicationAppliesNothing(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "hello\n"})
	digest := readDigest(t, ws, "a.txt")
	if err := os.Chmod(ws.Root(), 0o555); err != nil {
		t.Skipf("cannot make the directory read-only: %v", err)
	}
	t.Cleanup(func() { os.Chmod(ws.Root(), 0o700) })

	outcome := runTool(t, NewEditFileTool(ws), `{"path":"a.txt","expected_sha256":"`+
		digest+`","old_text":"hello","new_text":"goodbye"}`)

	if outcome.OK || outcome.Effect != EffectNone {
		t.Fatalf("got %+v", outcome)
	}
	if fileContent(t, ws, "a.txt") != "hello\n" {
		t.Fatal("the file changed despite a failed publication")
	}
}
