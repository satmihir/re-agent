package reagent

import (
	"bytes"
	"context"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestContextWindow_CapsKnownModelsAndAdmitsUnknown(t *testing.T) {
	for _, test := range []struct {
		model     string
		cap, want int64
	}{
		{"gpt-6-sol", 0, 1_050_000},
		{"gpt-6-sol", 70_000, 70_000},
		{"claude-haiku-4-5", 300_000, 200_000},
		{"unknown", 70_000, 70_000},
		{"unknown", 0, 0},
	} {
		cfg := Config{Model: test.model, ContextWindowTokens: test.cap}
		if got := effectiveContextWindow(cfg); got != test.want {
			t.Fatalf("%s cap %d: got %d want %d", test.model, test.cap, got, test.want)
		}
	}
}

func TestMain_ContextWindowValidationAndTrace(t *testing.T) {
	for _, raw := range []string{"", "0", "-1", "70000x", "1.5", "9223372036854775808"} {
		t.Run(raw, func(t *testing.T) {
			t.Setenv("REAGENT_CONTEXT_WINDOW_TOKENS", raw)
			var out, errs bytes.Buffer
			code := Main(context.Background(), []string{"run", "--show-context", "task"}, strings.NewReader(""), &out, &errs)
			if code != exitUsage || !strings.Contains(errs.String(), "must be a positive integer") {
				t.Fatalf("exit=%d err=%s", code, errs.String())
			}
		})
	}
	t.Setenv("REAGENT_CONTEXT_WINDOW_TOKENS", "70000")
	dir := t.TempDir()
	script, trace := filepath.Join(dir, "script.json"), filepath.Join(dir, "trace.jsonl")
	if err := os.WriteFile(script, []byte(`[{"blocks":[{"kind":"text","text":"done"}]}]`), 0600); err != nil {
		t.Fatal(err)
	}
	var out, errs bytes.Buffer
	code := Main(context.Background(), []string{"run", "--workspace", dir, "--scripted", script, "--trace-file", trace, "task"}, strings.NewReader(""), &out, &errs)
	if code != exitOK {
		t.Fatalf("exit=%d err=%s", code, errs.String())
	}
	started := readEvents(t, trace)[0].Data.(map[string]any)
	if started["in_run_compact_min_shrink"] != inRunCompactMinShrink || started["context_window_tokens"] != float64(70_000) || started["context_window_override"] != float64(70_000) {
		t.Fatalf("trace=%+v", started)
	}
}

func TestSessionStore_ContextWindowSurvivesResume(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider, cfg.ContextWindowTokens, cfg.WorkspacePath = openaiName, 70_000, t.TempDir()
	s := NewSession(cfg, NewScriptedModel(), NewTrace(io.Discard), io.Discard)
	s.store = testSessionStore(t, s.ID)
	if err := s.checkpoint("idle", ""); err != nil {
		t.Fatal(err)
	}
	cp, err := s.store.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	restored := NewSession(testConfig(t), s.model, NewTrace(io.Discard), io.Discard)
	restored.restore(cp)
	if restored.cfg.ContextWindowTokens != 70_000 || newRun(restored, "run").window != 70_000 {
		t.Fatalf("resume cfg=%+v", restored.cfg)
	}
}

func TestChat_ContextWindowStatusAndAutomaticCompaction(t *testing.T) {
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.ContextWindowTokens = openaiName, "gpt-6-sol", 70_000
	s := NewSession(cfg, NewScriptedModel(), NewTrace(io.Discard), io.Discard)
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
	s.lastRequest = Usage{Known: true, InputTokens: 56_000}
	c := &conversation{session: s}
	var out bytes.Buffer
	c.commandStatus(&out)
	if !strings.Contains(out.String(), "70k override") || !strings.Contains(out.String(), "80% of the 70k window") || !c.shouldAutoCompact() || c.regionStatus().window != 70_000 {
		t.Fatalf("status=%s auto=%t region=%+v", out.String(), c.shouldAutoCompact(), c.regionStatus())
	}
}

func TestAgents_ContextWindowInherited(t *testing.T) {
	s := agentFixture(t)
	s.cfg.ContextWindowTokens = 70_000
	var inherited int64
	s.agentModel = func(cfg Config, _ *Trace) (Model, error) {
		inherited = cfg.ContextWindowTokens
		return NewScriptedModel(turn(textBlock("done"))), nil
	}
	spawned := false
	s.model = agentTestModel{generate: func(context.Context, ModelRequest) (ModelResponse, error) {
		if !spawned {
			spawned = true
			return turn(spawnBlock("spawn", "inspect the project", "")), nil
		}
		return turn(textBlock("parent done")), nil
	}}
	agentTurn(t, s, "task")
	if inherited != 70_000 {
		t.Fatalf("child override=%d", inherited)
	}
}

func TestModelSwitch_ContextWindowAdmission(t *testing.T) {
	c := newConversation(t, "gpt-6-sol", "low", 1)
	c.session.cfg.ContextWindowTokens = 70_000
	c.session.history[0].User.Text = strings.Repeat("x", 150_000)
	info, _ := findModel("gpt-6-luna")
	if _, err := c.transitionModel(context.Background(), info, false); err == nil {
		t.Fatal("model switch ignored the testing window")
	}
	if c.session.cfg.Model != "gpt-6-sol" {
		t.Fatal("failed admission changed the model")
	}
	c.session.history[0].User.Text = "small task"
	if _, err := c.transitionModel(context.Background(), info, false); err != nil {
		t.Fatal(err)
	}
	if c.session.cfg.Model != info.ID || newRun(c.session, "run").window != 70_000 {
		t.Fatal("successful switch lost the override")
	}
}

func TestAuto_ContextWindowAdmission(t *testing.T) {
	cfg := autoConfig(t, NewEchoTool())
	cfg.ContextWindowTokens = 70_000
	s := autoSession(t, cfg, NewScriptedModel(), NewScriptedModel(), newFakeAPI(t, autoChoice("fast", 0.95)))
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: strings.Repeat("x", 150_000)}}}
	if _, err := stageAutoCandidate(context.Background(), s, s.auto.routes[0]); err == nil {
		t.Fatal("automatic switch ignored the testing window")
	}
}
