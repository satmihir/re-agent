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
		{"gpt-6.1-sol", 0, 1_050_000},
		{"gpt-6.1-sol", 70_000, 70_000},
		{"claude-sonnet-5-5", 2_000_000, 1_000_000},
		{"claude-haiku-5-5", 0, 0},
		{"claude-haiku-5-5", 300_000, 300_000},
		{"unknown", 70_000, 70_000},
		{"unknown", 0, 0},
	} {
		cfg := Config{Model: test.model, ContextWindowTokens: test.cap}
		if got := effectiveContextWindow(cfg); got != test.want {
			t.Fatalf("%s cap %d: got %d want %d", test.model, test.cap, got, test.want)
		}
	}
}

func TestMain_ContextWindowEnvironment(t *testing.T) {
	t.Setenv("REAGENT_CONTEXT_GC_AGE_STEPS", "")
	t.Setenv("REAGENT_CONTEXT_GC_CONSOLIDATE", "")
	for _, raw := range []string{"", "70000", "0", "-1", "+70000", " 70000", "70000 ", "0x10", "private-window", "1.5", "9223372036854775808"} {
		t.Run(raw, func(t *testing.T) {
			t.Setenv("REAGENT_CONTEXT_WINDOW_TOKENS", raw)
			var out, errs bytes.Buffer
			code := Main(context.Background(), []string{"run", "--workspace", t.TempDir(), "--show-context", "task"}, strings.NewReader(""), &out, &errs)
			if raw == "" || raw == "70000" {
				if code != exitOK || out.Len() == 0 || errs.Len() != 0 {
					t.Fatalf("exit=%d out=%s err=%s", code, out.String(), errs.String())
				}
				return
			}
			if code != exitUsage || out.Len() != 0 || errs.String() != "error: REAGENT_CONTEXT_WINDOW_TOKENS must be a positive integer\n" {
				t.Fatalf("exit=%d out=%s err=%s", code, out.String(), errs.String())
			}
		})
	}
}

func TestMain_ContextWindowTrace(t *testing.T) {
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
	cfg.Provider, cfg.Model, cfg.ContextWindowTokens = openaiName, "gpt-6.1-sol", 70_000
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
	c := newConversation(t, "gpt-6.1-sol", "low", 1)
	c.session.cfg.ContextWindowTokens = 70_000
	c.session.history[0].User.Text = strings.Repeat("x", 150_000)
	info, _ := findModel("gpt-6.1-luna")
	if _, err := c.transitionModel(context.Background(), info, false); err == nil {
		t.Fatal("model switch ignored the testing window")
	}
	if c.session.cfg.Model != "gpt-6.1-sol" {
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

func TestMain_ContextEnvironmentValidatedOnResume(t *testing.T) {
	for _, test := range []struct {
		name, window, age, consolidate, message string
	}{
		{"window", "private-window", "", "", "REAGENT_CONTEXT_WINDOW_TOKENS must be a positive integer"},
		{"age", "", "private-age", "", "REAGENT_CONTEXT_GC_AGE_STEPS must be 0 or an integer greater than 3"},
		{"consolidate", "", "", "private-bool", "REAGENT_CONTEXT_GC_CONSOLIDATE must be true, false, 1 or 0"},
		{"exclusive", "", "16", "true", "choose only one context GC experiment"},
	} {
		t.Run(test.name, func(t *testing.T) {
			t.Setenv("REAGENT_CONTEXT_WINDOW_TOKENS", test.window)
			t.Setenv("REAGENT_CONTEXT_GC_AGE_STEPS", test.age)
			t.Setenv("REAGENT_CONTEXT_GC_CONSOLIDATE", test.consolidate)
			var out, errs bytes.Buffer
			code := Main(context.Background(), []string{"chat", "--resume", "0000000000000000"}, strings.NewReader(""), &out, &errs)
			if code != exitUsage || out.Len() != 0 || errs.String() != "error: "+test.message+"\n" {
				t.Fatalf("exit=%d out=%s err=%s", code, out.String(), errs.String())
			}
		})
	}
}

func TestMain_ContextEnvironmentDoesNotOverrideResume(t *testing.T) {
	for _, test := range []struct {
		name              string
		gc                bool
		age               int
		consolidate       bool
		envAge, envConsol string
	}{
		{"disabled", false, 0, false, "16", "false"},
		{"age", true, 16, false, "0", "true"},
		{"consolidate", true, 0, true, "32", "false"},
	} {
		t.Run(test.name, func(t *testing.T) {
			t.Setenv("OPENAI_API_KEY", "test")
			t.Setenv("REAGENT_OPENAI_AUTH", "key")
			t.Setenv("API_PROXY_URL", "")
			t.Setenv("API_PROXY_PROVIDER", "")
			t.Setenv("REAGENT_CONTEXT_WINDOW_TOKENS", "90000")
			t.Setenv("REAGENT_CONTEXT_GC_AGE_STEPS", test.envAge)
			t.Setenv("REAGENT_CONTEXT_GC_CONSOLIDATE", test.envConsol)
			cfg := testConfig(t)
			cfg.Provider, cfg.Model, cfg.WorkspacePath = openaiName, "gpt-6.1-sol", t.TempDir()
			cfg.ContextWindowTokens, cfg.ContextGC = 70_000, test.gc
			cfg.ContextGCAgeSteps, cfg.ContextGCConsolidate = test.age, test.consolidate
			s := NewSession(cfg, NewScriptedModel(), NewTrace(io.Discard), io.Discard)
			s.store = testSessionStore(t, s.ID)
			if err := s.checkpoint("idle", ""); err != nil {
				t.Fatal(err)
			}
			s.store.close()
			var out, errs bytes.Buffer
			code := Main(context.Background(), []string{"chat", "--resume", s.ID}, strings.NewReader("/plan\n/status\n/exit\n"), &out, &errs)
			if code != exitOK || !strings.Contains(errs.String(), "70k override") {
				t.Fatalf("exit=%d err=%s", code, errs.String())
			}
			cp, err := s.store.load(s.ID)
			if err != nil {
				t.Fatal(err)
			}
			if !cp.Plan || cp.ContextWindowTokens != 70_000 || cp.ContextGC != test.gc || cp.ContextGCAgeSteps != test.age || cp.ContextGCConsolidate != test.consolidate {
				t.Fatalf("resumed checkpoint=%+v", cp)
			}
		})
	}
}
