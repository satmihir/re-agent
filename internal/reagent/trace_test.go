package reagent

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
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
	scan.Buffer(make([]byte, 4096), 2*MaxRequestBytes)
	for scan.Scan() {
		var e event
		if err := json.Unmarshal(scan.Bytes(), &e); err != nil {
			t.Fatalf("line is not valid JSON: %v", err)
		}
		events = append(events, e)
	}
	if err := scan.Err(); err != nil {
		t.Fatalf("read trace: %v", err)
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

// v0 §7: every literal event emitted by the harness needs a documented data shape.
func TestTrace_WrittenEventTypesDocumented(t *testing.T) {
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("locate trace test source")
	}
	root := filepath.Join(filepath.Dir(file), "../..")
	doc, err := os.ReadFile(filepath.Join(root, "docs/reagent-trace-format.md"))
	if err != nil {
		t.Fatal(err)
	}
	files, err := filepath.Glob(filepath.Join(root, "internal/reagent/*.go"))
	if err != nil {
		t.Fatal(err)
	}
	written := regexp.MustCompile(`\.Write\("([a-z]+(?:\.[a-z]+)+)"`)
	found := make(map[string]bool)
	for _, path := range files {
		if strings.HasSuffix(path, "_test.go") {
			continue
		}
		source, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		for _, match := range written.FindAllSubmatch(source, -1) {
			kind := string(match[1])
			found[kind] = true
			if !strings.Contains(string(doc), "| `"+kind+"` |") {
				t.Errorf("%s writes undocumented event %s", filepath.Base(path), kind)
			}
		}
	}
	if len(found) == 0 {
		t.Fatal("found no event writers")
	}
}

func TestTrace_AgentEventsAreParentLocalAndTraceContinues(t *testing.T) {
	s := agentFixture(t)
	s.model = NewScriptedModel(turn(spawnBlock("spawn", "private-child-task", "named")), turn(callBlock("wait", "wait", `{"ids":["named"]}`)), turn(textBlock("done")))
	result := agentTurn(t, s, "task")
	childPath := s.agentRoster()[0].TracePath
	s.model = NewScriptedModel(turn(callBlock("send", "send", `{"name":"named","message":"follow-up"}`)), turn(callBlock("wait2", "wait", `{"ids":["named"]}`)), turn(textBlock("done")))
	second := agentTurn(t, s, "later")
	kinds := map[string]int{}
	for _, path := range []string{result.TracePath, second.TracePath} {
		for _, e := range readEvents(t, path) {
			kinds[e.Type]++
			if e.ParentID != "" {
				t.Fatal("root trace marked as child")
			}
			if e.Type == "model.requested" {
				data := e.Data.(map[string]any)
				if data["scope"].(map[string]any)["session_id"] != s.ID {
					t.Fatal("child request leaked to parent trace")
				}
			}
		}
	}
	if kinds["agent.spawn"] != 1 || kinds["agent.send"] != 1 || kinds["agent.result"] != 2 {
		t.Fatalf("parent events %+v", kinds)
	}
	runs := map[string]bool{}
	for i, e := range readEvents(t, childPath) {
		if e.Seq != i+1 || e.ParentID != "root" {
			t.Fatalf("agent envelope %+v", e)
		}
		runs[e.RunID] = true
	}
	if len(runs) != 2 || s.agentRoster()[0].TracePath != childPath {
		t.Fatal("named agent did not keep one trace")
	}
}
