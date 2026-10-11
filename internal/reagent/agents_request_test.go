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
		{openaiName, false, "6aebc279f4c6c9c8dafca16de615d1abd9d0b91fee017b17d117923a68cedb93"},
		{openaiName, true, "29fdace403ea4e4b3c32e5ea2837b2071825b2a6c5f5ff0a2f7d86f026e8a693"},
		{anthropicName, false, "30aad69c5ca3288d4b5921f55f2580fc59a8ea625e00db750145ab83cb05889d"},
		{anthropicName, true, "1829741f16cb3249dde83b4ec3a6d33a2aacad602bcd12d3ba0632494acb2eaa"},
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
