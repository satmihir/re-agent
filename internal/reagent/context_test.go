package reagent

import (
	"runtime"
	"strings"
	"testing"
)

func TestContext_FrictionInstructionsOnlyWithTheFlag(t *testing.T) {
	cfg := testConfig(t)
	plain := instructions(cfg)
	if strings.Contains(plain, "# Friction reports") {
		t.Fatal("friction instructions enabled by default")
	}
	want := `
# Friction reports

This session is testing re:agent itself. When the harness gets in your way,
call report_friction once, briefly, and carry on with the task: a tool error
whose message misled you, a capability you had to work around, a tool
description or instruction that was unclear, or harness behavior that looks
wrong. Cite the calls involved. Do not report your own mistakes unless the
harness made them likely, and do not stop the task to report.
`
	cfg.ReportFriction = true
	if got := instructions(cfg); got != plain+want {
		t.Fatalf("friction instructions: %q", got)
	}
	project := "project text\n"
	cfg.ProjectInstructions = &project
	if got := instructions(cfg); got != plain+want+"\n# Project instructions (AGENTS.md)\n\n"+project {
		t.Fatalf("instruction order: %q", got)
	}
	first := BuildContext(cfg, RequestScope{Step: 1}, nil)
	later := BuildContext(cfg, RequestScope{Step: 2}, nil)
	if first.Instructions != later.Instructions {
		t.Fatal("friction instructions changed between requests")
	}
	cfg.ReportFriction = false
	if got := instructions(cfg); got != plain+"\n# Project instructions (AGENTS.md)\n\n"+project {
		t.Fatalf("flag-off instructions changed: %q", got)
	}
}

func TestBuildContext_IsPureAndOrdered(t *testing.T) {
	cfg := testConfig(t, NewEchoTool(), countingTool{runs: new(int)})
	cfg.WorkspacePath = "/tmp/example"
	history := []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
	scope := RequestScope{SessionID: "s", RunID: "r", Step: 1}

	req := BuildContext(cfg, scope, history)
	again := BuildContext(cfg, scope, history)

	if req.Instructions != again.Instructions || !strings.Contains(req.Instructions, "re:agent") {
		t.Fatal("instructions are not a stable embedded value")
	}
	if len(req.Tools) != 2 || req.Tools[0].Name != "counter" || req.Tools[1].Name != "echo" {
		t.Fatalf("tools are not sorted by name: %+v", req.Tools)
	}
	if !strings.Contains(req.Instructions, "Workspace: "+cfg.WorkspacePath) {
		t.Fatal("the runtime section does not name the workspace")
	}
	if req.Scope != scope || req.Model != cfg.Model {
		t.Fatalf("got %+v", req)
	}
}

// v1 §5.2: a later append to the run's history must not reach a request that
// was already built.
func TestBuildContext_HistoryIsCopied(t *testing.T) {
	cfg := testConfig(t)
	history := make([]Entry, 1, 4)
	history[0] = Entry{Kind: EntryUser, User: &UserTurn{Text: "task"}}

	req := BuildContext(cfg, RequestScope{Step: 1}, history)
	history = append(history, Entry{Kind: EntryAssistant, Assistant: &ModelResponse{}})

	if len(req.History) != 1 {
		t.Fatalf("built request grew to %d entries", len(req.History))
	}
}

// The runtime section is the only thing the model knows about its own
// environment, so its whole contents are pinned here.
func TestInstructions_RuntimeSectionContents(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	registry, err := NewRegistry(Mode{}, NewReadFileTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{
		Provider: anthropicName, Model: "claude-haiku-4-5", ReasoningEffort: "low",
		Registry: registry, WorkspacePath: "/tmp/example", MaxSteps: 20, MaxToolCalls: 40,
	}
	text := instructions(cfg)
	section := text[strings.Index(text, "# Runtime"):]

	want := "# Runtime\n\n" +
		"Provider: anthropic\n" +
		"Model: claude-haiku-4-5\n" +
		"Reasoning effort: low\n" +
		"Platform: " + runtime.GOOS + "\n" +
		"Workspace: /tmp/example\n" +
		"Mode: read, write, and execute\n" +
		"Budget: 20 model requests and 40 tool calls per run\n"
	if section != want {
		t.Fatalf("got:\n%s\nwant:\n%s", section, want)
	}
}

func TestContext_ProjectInstructionsFollowTheRuntimeSection(t *testing.T) {
	cfg := testConfig(t)
	project := "Local convention: use make check.\n\n"
	plain := instructions(cfg)
	cfg.ProjectInstructions = &project
	got := instructions(cfg)
	if got != plain+"\n# Project instructions (AGENTS.md)\n\n"+project {
		t.Fatalf("project section does not follow runtime exactly:\n%q", got)
	}
	first := BuildContext(cfg, RequestScope{Step: 1}, nil)
	later := BuildContext(cfg, RequestScope{Step: 2}, nil)
	if first.Instructions != later.Instructions {
		t.Fatal("project instructions changed between steps")
	}
	empty := ""
	cfg.ProjectInstructions = &empty
	if got := instructions(cfg); got != plain+"\n# Project instructions (AGENTS.md)\n\n" {
		t.Fatalf("an empty but present file was omitted: %q", got)
	}
}

// An unset effort means no such parameter is sent, and the section says so
// rather than naming a value the harness did not choose.
func TestInstructions_UnsetEffortIsNamedAsTheProvidersDefault(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": "x"})
	registry, err := NewRegistry(Mode{}, NewReadFileTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	text := instructions(Config{Registry: registry})
	if !strings.Contains(text, "Reasoning effort: the provider's default\n") {
		t.Fatalf("got runtime section:\n%s", text[strings.Index(text, "# Runtime"):])
	}
}

// Nothing in the runtime section may vary between steps. A prefix that changed
// per request would be a cache miss every time, on both providers.
func TestInstructions_DoNotVaryBetweenSteps(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.WorkspacePath = openaiName, "gpt-x", "/tmp/example"
	history := []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}

	first := BuildContext(cfg, RequestScope{SessionID: "s", RunID: "r", Step: 1}, history)
	later := BuildContext(cfg, RequestScope{SessionID: "s", RunID: "r", Step: 7}, history)

	if first.Instructions != later.Instructions {
		t.Fatalf("instructions drifted between steps:\n%q\n%q", first.Instructions, later.Instructions)
	}
	for _, forbidden := range []string{"Step", "step 1", "remaining", "elapsed"} {
		if strings.Contains(first.Instructions[strings.Index(first.Instructions, "# Runtime"):], forbidden) {
			t.Fatalf("the runtime section carries %q, which changes between steps", forbidden)
		}
	}
}
