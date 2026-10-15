package reagent

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"runtime"
	"strings"
	"testing"
)

func TestAgents_DisabledRequestBytes(t *testing.T) {
	// Git inspection, deletion's public contract and review guidance intentionally
	// change all modes. Freeze those bytes with agents off, including cache keys.
	// Normalize Darwin before encoding without excluding any request fields.
	for _, scenario := range []struct {
		provider string
		readOnly bool
		digest   string
	}{
		{openaiName, false, "6a522f77ee047bed903fde40494aa063d6bb17887bd099c3ad929a043cc1f215"},
		{openaiName, true, "debeeae4c855561849d882ab24f674fea921227c905ddbfce27cd32baca8d9c1"},
		{anthropicName, false, "581b698d8e551b3ba49ae1f1b79d988d2f28105dcdbac22636ce0105bd262648"},
		{anthropicName, true, "13a10d400c2e4c810614ba09459986bb5f4de15e3a66ba4f203091fb5a3e66e9"},
	} {
		t.Run(fmt.Sprintf("%s/%t", scenario.provider, scenario.readOnly), func(t *testing.T) {
			registry, err := NewRegistry(Mode{ReadOnly: scenario.readOnly}, NewListFilesTool(nil), NewReadFileTool(nil), NewSearchTextTool(nil), NewGitInspectTool(nil), NewEditFileTool(nil), NewWriteFileTool(nil), NewDeleteFileTool(nil), NewExecTool(nil), NewRequestWorkspaceAccessTool(), NewSwitchWorkspaceTool())
			if err != nil {
				t.Fatal(err)
			}
			model := "gpt-6-sol"
			if scenario.provider == anthropicName {
				model = "claude-sonnet-5-5"
			}
			cfg := Config{Provider: scenario.provider, Model: model, ReasoningEffort: "low", WorkspacePath: "/workspace", Registry: registry}
			history := []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task", Workspace: json.RawMessage(`{"kind":"workspace_state","workspace":"/workspace","date":"2026-10-10"}`)}}}
			req := BuildContext(cfg, RequestScope{Step: 1}, history)
			req.Instructions = strings.Replace(req.Instructions, "\nPlatform: "+runtime.GOOS+"\n", "\nPlatform: darwin\n", 1)
			raw, err := encodeRequest(cfg, req)
			if err != nil {
				t.Fatal(err)
			}
			for _, spec := range req.Tools {
				if spec.Name == "spawn" || spec.Name == "wait" || spec.Name == "threads" {
					t.Fatal("agent declarations leaked with agents off")
				}
			}
			if strings.Contains(req.Instructions, "authority granted by your parent") {
				t.Fatal("agent instructions leaked")
			}
			if got := fmt.Sprintf("%x", sha256.Sum256(raw)); got != scenario.digest {
				t.Fatalf("agents-off request bytes changed: %s != frozen %s", got, scenario.digest)
			}
		})
	}
}
