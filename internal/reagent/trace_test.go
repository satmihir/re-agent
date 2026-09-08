package reagent

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func readEvents(t *testing.T, path string) []event {
	t.Helper()
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()

	var events []event
	scan := bufio.NewScanner(f)
	for scan.Scan() {
		var e event
		if err := json.Unmarshal(scan.Bytes(), &e); err != nil {
			t.Fatalf("line is not valid JSON: %v", err)
		}
		events = append(events, e)
	}
	return events
}

func TestTrace_RecordsOneRunInOrder(t *testing.T) {
	path := filepath.Join(t.TempDir(), "events.jsonl")
	trace := NewTrace(os.Stderr)
	oneTurn(t, context.Background(), testConfig(t), NewScriptedModel(
		turn(callBlock("call_1", "echo", `{"text":"hi"}`)),
		turn(textBlock("done"))), trace, path, "task")

	var kinds []string
	for i, e := range readEvents(t, path) {
		if e.Seq != i+1 || e.SchemaVersion != traceSchemaVersion || e.RunID != "run" {
			t.Fatalf("bad envelope at %d: %+v", i, e)
		}
		kinds = append(kinds, e.Type)
	}
	want := "run.started model.requested model.accepted tool.started tool.finished model.requested model.accepted run.finished"
	if got := strings.Join(kinds, " "); got != want {
		t.Fatalf("got  %s\nwant %s", got, want)
	}
}

// v0 §7 drops I13 on purpose: a trace that cannot be written warns and the run
// continues, rather than stopping the work.
func TestTrace_WriteFailureWarnsAndRunContinues(t *testing.T) {
	blocked := filepath.Join(t.TempDir(), "file")
	if err := os.WriteFile(blocked, nil, 0o600); err != nil {
		t.Fatal(err)
	}

	var warnings bytes.Buffer
	_, result := oneTurn(t, context.Background(), testConfig(t), NewScriptedModel(turn(textBlock("done"))),
		NewTrace(&warnings), filepath.Join(blocked, "events.jsonl"), "task")

	if result.Status != StatusCompleted {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	if result.TracePath != "" {
		t.Fatalf("a disabled trace must not report a path, got %q", result.TracePath)
	}
	if !strings.Contains(warnings.String(), "tracing disabled") {
		t.Fatalf("no warning was printed: %q", warnings.String())
	}
}
