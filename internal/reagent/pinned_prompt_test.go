package reagent

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// anthropicSystem is the encoded system field, which Anthropic binds
// thinking-block signatures to.
func anthropicSystem(t *testing.T, req ModelRequest) string {
	t.Helper()
	body, err := EncodeAnthropicRequest(req)
	if err != nil {
		t.Fatal(err)
	}
	var decoded struct {
		System json.RawMessage `json:"system"`
	}
	if err := json.Unmarshal(body, &decoded); err != nil {
		t.Fatal(err)
	}
	return string(decoded.System)
}

func pinnedFixture(t *testing.T) (*Session, string, string) {
	t.Helper()
	root, target := t.TempDir(), t.TempDir()
	for dir, text := range map[string]string{root: "launch rules", target: "target rules"} {
		if err := os.WriteFile(filepath.Join(dir, "AGENTS.md"), []byte(text), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	s := workspaceSession(t, root, Mode{}, NewScriptedModel())
	s.cfg.Provider, s.cfg.Model, s.cfg.ReasoningEffort = anthropicName, "claude-opus-5-5", "high"
	pinInstructions(&s.cfg)
	s.workspaceConsent = func(context.Context, workspaceDestination) (bool, error) { return true, nil }
	return s, root, target
}

// Session 43b5389c8344444b failed with HTTP 400 at the first request after a
// switch: every earlier thinking block was bound to the old system prompt.
func TestPinnedPrompt_WorkspaceSwitchKeepsAnthropicSystemBytes(t *testing.T) {
	s, _, target := pinnedFixture(t)
	before := anthropicSystem(t, BuildContext(s.cfg, RequestScope{}, nil))
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	after := anthropicSystem(t, BuildContext(s.cfg, RequestScope{}, nil))
	if before != after || !strings.Contains(after, "launch rules") || strings.Contains(after, "target rules") {
		t.Fatal("the switch changed the system prompt")
	}
}

func TestPinnedPrompt_EffortChangeKeepsTheSystemPrompt(t *testing.T) {
	s, _, _ := pinnedFixture(t)
	before := BuildContext(s.cfg, RequestScope{}, nil)
	s.SetEffort("low")
	after := BuildContext(s.cfg, RequestScope{}, nil)
	if after.Instructions != before.Instructions || after.ReasoningEffort != "low" {
		t.Fatal("effort change should alter the parameter, not the prompt")
	}
}

func TestPinnedPrompt_ResumeRestoresTheRecordedText(t *testing.T) {
	s, _, target := pinnedFixture(t)
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	s.SetEffort("low")
	recorded := instructions(s.cfg)
	s.store = &sessionStore{dir: t.TempDir()}
	if err := s.checkpoint("terminal", ""); err != nil {
		t.Fatal(err)
	}
	cp, err := s.store.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	// A resumed session re-renders from different settings, then restores the record.
	resumed := NewSession(s.cfg, nil, NewTrace(io.Discard), io.Discard)
	resumed.cfg.prompt = nil
	pinInstructions(&resumed.cfg)
	if instructions(resumed.cfg) == recorded {
		t.Fatal("fixture does not distinguish a re-render from the record")
	}
	resumed.restore(cp)
	if instructions(resumed.cfg) != recorded {
		t.Fatal("resume did not restore the recorded prompt")
	}
	// A checkpoint from before pinning keeps the launch rendering.
	legacy := NewSession(s.cfg, nil, NewTrace(io.Discard), io.Discard)
	cp.Prompt = nil
	rendered := instructions(legacy.cfg)
	legacy.restore(cp)
	if instructions(legacy.cfg) != rendered {
		t.Fatal("legacy checkpoint changed the pin")
	}
}

func TestPinnedPrompt_SummaryAndResetRepinToTheActiveWorkspace(t *testing.T) {
	s, _, target := pinnedFixture(t)
	if outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target}); !outcome.OK {
		t.Fatal(outcome)
	}
	s.Reset()
	text := instructions(s.cfg)
	if !strings.Contains(text, "target rules") || strings.Contains(text, "launch rules") || !strings.Contains(text, "Workspace: "+s.cfg.WorkspacePath) {
		t.Fatal("reset kept the stale pin")
	}
}

func TestPinnedPrompt_ChildSessionsPinTheirOwnPrompt(t *testing.T) {
	s, _, _ := pinnedFixture(t)
	cfg := s.cfg
	cfg.agent = true
	child := NewSession(cfg, nil, NewTrace(io.Discard), io.Discard)
	if instructions(child.cfg) == instructions(s.cfg) || !strings.Contains(instructions(child.cfg), "# Agent task") {
		t.Fatal("child inherited the parent's pin")
	}
	if strings.Contains(instructions(child.cfg), "switch_workspace result") {
		t.Fatal("agents cannot switch workspaces; the note does not apply")
	}
}

func TestPinnedPrompt_OversizedAgentsFileIsReferredNotDropped(t *testing.T) {
	s, _, target := pinnedFixture(t)
	// Loadable, but too large to fit beside the snapshot in one result.
	if err := os.WriteFile(filepath.Join(target, "AGENTS.md"), []byte(strings.Repeat("rule\n", (MaxResultBytes-512)/5)), 0o600); err != nil {
		t.Fatal(err)
	}
	outcome := workspaceTool(t, s, "switch_workspace", map[string]string{"path": target})
	if !outcome.OK {
		t.Fatalf("an oversized AGENTS.md must not fail the switch: %+v", outcome)
	}
	var switched workspaceSwitchResult
	data(t, outcome, &switched)
	if switched.ProjectInstructions != nil || encodedSize(outcome) > MaxResultBytes {
		t.Fatal("an AGENTS.md that cannot fit must be referred to, not inlined")
	}
	if !strings.Contains(switched.ProjectInstructionsNote, "read it with read_file") {
		t.Fatalf("note %q", switched.ProjectInstructionsNote)
	}
}
