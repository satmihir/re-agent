package reagent

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"runtime"
	"strings"
	"testing"
)

func TestAgents_DisabledHistoricalRequestBytes(t *testing.T) {
	// Stage-1 hashes (5ca87b6) used Darwin; normalize before encoding so cache
	// keys are normalized too, without excluding any request field from the check.
	for _, scenario := range []struct {
		provider string
		readOnly bool
		digest   string
	}{
		{openaiName, false, "a9e495caa69fb0905582cd3bbc5bdbf702101c412caf5642031cb783b8ed7cb9"},
		{openaiName, true, "be5aae284dc05536b0d34baec199fa5b4cfbf9557244e2b44c1db35a88f13376"},
		{anthropicName, false, "f521d38eb4c11ca749c3c0d75bbc5f37f428e529463c06337a8cd1787150027f"},
		{anthropicName, true, "f0d4cd5258cf7da2bf4b5928217ceefbd4da60bb8548ac7d8a3c6a3084d01f9f"},
	} {
		t.Run(fmt.Sprintf("%s/%t", scenario.provider, scenario.readOnly), func(t *testing.T) {
			registry, err := NewRegistry(Mode{ReadOnly: scenario.readOnly}, NewListFilesTool(nil), NewReadFileTool(nil), NewSearchTextTool(nil), NewEditFileTool(nil), NewWriteFileTool(nil), NewDeleteFileTool(nil), NewExecTool(nil), NewRequestWorkspaceAccessTool(), NewSwitchWorkspaceTool())
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
			if got := fmt.Sprintf("%x", sha256.Sum256(raw)); got != scenario.digest {
				t.Fatalf("agents-off request bytes changed: %s != historical %s", got, scenario.digest)
			}
		})
	}
}
