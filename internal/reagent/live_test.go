package reagent

import (
	"context"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// TestLive_ReadsAMarkerFromTheWorkspace is the opt-in conformance check: it is
// the only test that contacts the real API, and it runs only when explicitly
// enabled. It verifies that the deployed API accepts the request v0 encodes and
// completes one tool round trip. It says nothing about model quality.
func TestLive_ReadsAMarkerFromTheWorkspace(t *testing.T) {
	if os.Getenv("REAGENT_LIVE_TESTS") != "1" {
		t.Skip("live conformance test not enabled: set REAGENT_LIVE_TESTS=1 and OPENAI_API_KEY")
	}
	apiKey := os.Getenv("OPENAI_API_KEY")
	if apiKey == "" {
		t.Skip("live conformance test enabled but OPENAI_API_KEY is not set")
	}
	t.Log("contacting the real API; this test spends tokens")

	const marker = "zubrowka-4417"
	ws := testWorkspace(t, map[string]string{"notes.txt": "project marker: " + marker + "\n"})
	registry, err := NewRegistry(Mode{}, NewListFilesTool(ws), NewReadFileTool(ws), NewSearchTextTool(ws))
	if err != nil {
		t.Fatal(err)
	}

	tracePath := filepath.Join(t.TempDir(), "events.jsonl")
	trace := OpenTrace(tracePath, "session", "run", io.Discard)
	defer trace.Close()

	cfg := Config{
		Model: resolveModel(os.Getenv("REAGENT_TEST_MODEL")), Registry: registry,
		WorkspacePath: ws.Root(), MaxSteps: 4, MaxToolCalls: 6,
	}
	model := NewOpenAIModel(apiKey, "", NewHTTPClient(), trace)
	run := NewRun(cfg, model, trace, "session", "run", io.Discard)
	result := run.Execute(context.Background(), "What is the project marker in this workspace?")

	if result.Status != StatusCompleted {
		t.Fatalf("got %s: %s", result.Status, result.Reason)
	}
	if !strings.Contains(result.Reply, marker) {
		t.Fatalf("the reply does not contain the marker it had to read: %q", result.Reply)
	}
	if result.ToolCalls == 0 {
		t.Fatal("the model answered without reading the workspace")
	}
	if !result.Usage.Known || result.Usage.InputTokens == 0 {
		t.Fatalf("no usage was reported: %+v", result.Usage)
	}

	// Every accepted turn must have carried provider items forward.
	for _, entry := range run.history {
		if entry.Kind == EntryAssistant && len(entry.Assistant.Native.Items) == 0 {
			t.Fatal("an accepted turn retained no provider items for continuation")
		}
	}
	t.Logf("live trace: %s", tracePath)
}
