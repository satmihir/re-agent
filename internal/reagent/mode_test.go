package reagent

import (
	"strings"
	"testing"
)

func TestRegistry_ModeDecidesWhatTheModelIsToldAbout(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	tools := []Tool{NewReadFileTool(ws), NewEditFileTool(ws)}

	readOnly, err := NewRegistry(Mode{}, tools...)
	if err != nil {
		t.Fatal(err)
	}
	if len(readOnly.Specs()) != 1 || readOnly.Specs()[0].Name != "read_file" {
		t.Fatalf("edit_file was declared in read-only mode: %+v", readOnly.Specs())
	}
	// Withheld, but still known, so a call for it can be refused precisely.
	if _, active := readOnly.Lookup("edit_file"); active {
		t.Fatal("edit_file is active in read-only mode")
	}
	if !readOnly.known("edit_file") {
		t.Fatal("edit_file is not remembered as a tool this build has")
	}

	writable, err := NewRegistry(Mode{AllowWrite: true}, tools...)
	if err != nil {
		t.Fatal(err)
	}
	if len(writable.Specs()) != 2 {
		t.Fatalf("got %d tools in write mode", len(writable.Specs()))
	}
}

// A call for a withheld tool is refused for that reason, and the file is not
// touched. Nothing the model sends can widen the mode (v1 §10.3).
func TestLoop_WithheldToolIsPermissionDeniedNotMissing(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"main.go": source})
	registry, err := NewRegistry(Mode{}, NewReadFileTool(ws), NewEditFileTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Model: "test", Registry: registry, WorkspacePath: ws.Root(), MaxSteps: 20, MaxToolCalls: 40}

	run, result := runScript(t, cfg,
		turn(callBlock("call_1", "edit_file", `{"path":"main.go","expected_sha256":"x","old_text":"a","new_text":"b"}`),
			callBlock("call_2", "no_such_tool", `{}`)),
		turn(textBlock("I cannot edit in this mode.")))

	if result.Status != StatusCompleted {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	got := results(run)
	if got[0].Outcome.Code != "permission_denied" {
		t.Fatalf("got %s (%s)", got[0].Outcome.Code, got[0].Outcome.Message)
	}
	if !strings.Contains(got[0].Outcome.Message, "read only") {
		t.Fatalf("the refusal does not name the mode: %s", got[0].Outcome.Message)
	}
	if got[1].Outcome.Code != "tool_unavailable" {
		t.Fatalf("an unknown name got %s", got[1].Outcome.Code)
	}
	if fileContent(t, ws, "main.go") != source {
		t.Fatal("a withheld tool changed the workspace")
	}
	// Nothing ran, so nothing is reported as changed.
	if len(result.Effects) != 0 {
		t.Fatalf("got effects %+v", result.Effects)
	}
}

// A run that stops after an edit still reports the edit it made (v1 §19.3).
func TestLoop_ReportsEffectsEvenWhenTheRunFails(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"main.go": source})
	registry, err := NewRegistry(Mode{AllowWrite: true}, NewEditFileTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Model: "test", Registry: registry, WorkspacePath: ws.Root(), MaxSteps: 20, MaxToolCalls: 40}
	digest := digestOfFile(t, ws, "main.go")

	// The script stops after the edit, so the run ends without a final reply.
	_, result := runScript(t, cfg, turn(callBlock("call_1", "edit_file",
		`{"path":"main.go","expected_sha256":"`+digest+`","old_text":"timeout = 0","new_text":"timeout = 30"}`)))

	if result.Status != StatusProtocolError {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	if len(result.Effects) != 1 {
		t.Fatalf("got %+v", result.Effects)
	}
	effect := result.Effects[0]
	if effect.Effect != EffectApplied || effect.Tool != "edit_file" || effect.CallID != "call_1" {
		t.Fatalf("got %+v", effect)
	}
	// The summary names the target without dumping replacement text.
	if !strings.Contains(effect.Summary, "path=main.go") {
		t.Fatalf("got summary %q", effect.Summary)
	}
	if strings.Contains(effect.Summary, "expected_sha256") {
		t.Fatalf("the summary carries the digest: %q", effect.Summary)
	}
	if !strings.Contains(fileContent(t, ws, "main.go"), "timeout = 30") {
		t.Fatal("the reported edit did not reach the file")
	}
}

func TestInstructions_NameTheWorkspaceAndMode(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	for name, mode := range map[string]Mode{
		"read only":      {},
		"read and write": {AllowWrite: true},
	} {
		t.Run(name, func(t *testing.T) {
			registry, err := NewRegistry(mode, NewReadFileTool(ws))
			if err != nil {
				t.Fatal(err)
			}
			text := instructions(Config{Registry: registry, WorkspacePath: "/tmp/example"})
			if !strings.Contains(text, "Workspace: /tmp/example") || !strings.Contains(text, "Mode: "+name) {
				t.Fatalf("got runtime section:\n%s", text[strings.Index(text, "# Runtime"):])
			}
		})
	}
}
