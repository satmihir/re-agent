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
		{openaiName, false, "9e6ea1424b306046f8b252b152d82137d32b7110f5ee6a83cca6d51ab8cde1ee"},
		{openaiName, true, "52ce48aaf30fa9586149a5d593ccad041839f5c8ecf4f8269d68c6a393609fc2"},
		{anthropicName, false, "f5b9a9aa58c3aca47ba3bfe7416eeaef76d9dbf052d951316cbb2d14764e521b"},
		{anthropicName, true, "b3a900d466ff8a363dd21e72d1aad18f22f6c0fccd4477c1a7705b422cb8fb59"},
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
