package reagent

import (
	"bytes"
	"context"
	"io"
	"strings"
	"testing"
)

// chatSession drives the REPL with canned input and returns what it printed.
func chatSession(t *testing.T, model Model, input string) (stdout, stderr string) {
	t.Helper()
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	if code := chat(context.Background(), c, strings.NewReader(input), &out, &errs); code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, errs.String())
	}
	return out.String(), errs.String()
}

func TestChat_EachLineIsATurnOfOneConversation(t *testing.T) {
	stdout, stderr := chatSession(t, NewScriptedModel(
		turn(callBlock("call_1", "echo", `{"text":"hi"}`)),
		turn(textBlock("first")),
		turn(textBlock("second"))),
		"what is this?\n\n   \nand then?\n")

	if stdout != "first\nsecond\n" {
		t.Fatalf("stdout: %q", stdout)
	}
	// Diagnostics go to stderr, replies do not; no prompt is printed to a pipe.
	if !strings.Contains(stderr, "completed in 2 steps") || strings.Contains(stderr, "> ") {
		t.Fatalf("stderr: %q", stderr)
	}
}

func TestChat_CommandsAreLocalAndSpendNothing(t *testing.T) {
	model := NewScriptedModel(turn(textBlock("only reply")))
	stdout, stderr := chatSession(t, model,
		"/help\n/trace\n/bogus\nreal question\n/trace\n/exit\nnever sent\n")

	if stdout != "only reply\n" {
		t.Fatalf("stdout: %q", stdout)
	}
	for _, want := range []string{"/reset", "no run has been recorded yet", "unknown command /bogus", "events.jsonl"} {
		if !strings.Contains(stderr, want) {
			t.Fatalf("stderr lacks %q:\n%s", want, stderr)
		}
	}
	// The line after /exit was never read, so the script was never asked again.
	if model.next != 1 {
		t.Fatalf("the model was called %d times", model.next)
	}
}

func TestChat_BlockedSessionExplainsAndResetRecovers(t *testing.T) {
	stdout, stderr := chatSession(t, NewScriptedModel(
		turn(callBlock("call_1", "echo", `{"text":"hi"}`), callBlock("call_1", "echo", `{"text":"hi"}`)),
		turn(textBlock("after reset"))),
		"first\nsecond\n/reset\nthird\n")

	// "first" ends in a protocol error (a duplicate call id), "second" is
	// refused with directions, "third" works again on the fresh session.
	if stdout != "after reset\n" {
		t.Fatalf("stdout: %q", stdout)
	}
	if !strings.Contains(stderr, "protocol_error") || !strings.Contains(stderr, "use /reset") {
		t.Fatalf("stderr: %q", stderr)
	}
	if !strings.Contains(stderr, "fresh session") {
		t.Fatalf("stderr: %q", stderr)
	}
}

func TestChat_EOFExitsCleanly(t *testing.T) {
	stdout, _ := chatSession(t, NewScriptedModel(turn(textBlock("x"))), "")
	if stdout != "" {
		t.Fatalf("stdout: %q", stdout)
	}
}

func TestMain_ChatFlagsAreValidated(t *testing.T) {
	cases := map[string][]string{
		"chat with a prompt":  {"chat", "--scripted", "x.json", "a task"},
		"chat with preview":   {"chat", "--scripted", "x.json", "--show-context"},
		"chat with tracefile": {"chat", "--scripted", "x.json", "--trace-file", "/tmp/x"},
		"run with tracedir":   {"run", "--scripted", "x.json", "--trace-dir", "/tmp", "a task"},
		"unknown command":     {"plan", "a task"},
	}
	for name, args := range cases {
		t.Run(name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			if code := Main(context.Background(), args, strings.NewReader(""), &stdout, &stderr); code != exitUsage {
				t.Fatalf("exit %d, want %d: %s", code, exitUsage, stderr.String())
			}
		})
	}
}

// The whole thing through Main: a scripted chat from a pipe.
func TestMain_ScriptedChatFromAPipe(t *testing.T) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"chat",
		"--workspace", t.TempDir(), "--trace-dir", t.TempDir(),
		"--scripted", "../../testdata/scripts/echo_then_answer.json"},
		strings.NewReader("Where is the timeout set?\n/exit\n"), &stdout, &stderr)

	if code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}
	if got := strings.TrimSpace(stdout.String()); got != "The tool returned: the timeout is 30s." {
		t.Fatalf("stdout: %q", got)
	}
}

// newConversation builds a chat with both credentials and a transcript.
func newConversation(t *testing.T, model, effort string, turns int) *conversation {
	t.Helper()
	cfg := testConfig(t)
	info, _ := findModel(model)
	cfg.Provider, cfg.Model, cfg.ReasoningEffort = info.Provider, model, effort

	c := &conversation{
		cfg:      cfg,
		keys:     map[string]string{openaiName: "sk-openai", anthropicName: "sk-anthropic"},
		client:   NewHTTPClient(),
		trace:    NewTrace(io.Discard),
		traceDir: t.TempDir(),
		progress: io.Discard,
	}
	c.session = NewSession(cfg, NewScriptedModel(turn(textBlock("x"))), c.trace, io.Discard)
	for i := 0; i < turns; i++ {
		c.session.history = append(c.session.history,
			Entry{Kind: EntryUser, User: &UserTurn{Text: "q"}},
			Entry{Kind: EntryAssistant, Assistant: &ModelResponse{}})
	}
	return c
}

// A model change cannot continue an existing conversation, because the
// transcript holds items bound to the model that produced them.
func TestConversation_SwitchingModelStartsAFreshSession(t *testing.T) {
	c := newConversation(t, "gpt-5.6-luna", "xhigh", 3)
	before := c.session

	var stderr bytes.Buffer
	c.commandModel("claude-haiku-4-5", &stderr)

	if c.session == before || c.session.ID == before.ID {
		t.Fatal("the session was reused across a model change")
	}
	if len(c.session.history) != 0 || c.session.Turns() != 0 {
		t.Fatalf("the new session carries %d entries", len(c.session.history))
	}
	if c.cfg.Model != "claude-haiku-4-5" || c.cfg.Provider != anthropicName {
		t.Fatalf("got %+v", c.cfg)
	}
	// Effort resets to the new model's own default: Haiku rejects the value
	// the previous model was using.
	if c.cfg.ReasoningEffort != "" || c.session.cfg.ReasoningEffort != "" {
		t.Fatalf("effort survived the switch: %q", c.cfg.ReasoningEffort)
	}
	if !strings.Contains(stderr.String(), "3 turns discarded") {
		t.Fatalf("the switch did not say what it discarded: %q", stderr.String())
	}
}

func TestConversation_ModelSelectionRefusals(t *testing.T) {
	cases := map[string]struct {
		keys         map[string]string
		choice, want string
	}{
		"missing key": {
			map[string]string{openaiName: "sk-openai"},
			"claude-sonnet-5", "needs ANTHROPIC_API_KEY",
		},
		"already current": {
			map[string]string{openaiName: "sk-openai"},
			"gpt-5.6-luna", "already using",
		},
		"unknown name": {
			map[string]string{openaiName: "sk-openai"},
			"gpt-imaginary", "no model named",
		},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			conv := newConversation(t, "gpt-5.6-luna", "low", 2)
			conv.keys = c.keys
			before := conv.session

			var stderr bytes.Buffer
			conv.commandModel(c.choice, &stderr)

			if !strings.Contains(stderr.String(), c.want) {
				t.Fatalf("got %q, want it to mention %q", stderr.String(), c.want)
			}
			if conv.session != before {
				t.Fatal("a refused switch replaced the session")
			}
		})
	}
}

// Effort is a request parameter, not part of the transcript, so setting it
// keeps the conversation.
func TestConversation_EffortChangeKeepsTheConversation(t *testing.T) {
	c := newConversation(t, "claude-sonnet-5", "low", 2)
	before := c.session

	var stderr bytes.Buffer
	c.commandEffort("xhigh", &stderr)

	if c.cfg.ReasoningEffort != "xhigh" || c.session.cfg.ReasoningEffort != "xhigh" {
		t.Fatalf("got %q", c.cfg.ReasoningEffort)
	}
	if c.session != before || c.session.Turns() != 2 {
		t.Fatal("setting effort discarded the conversation")
	}
	if !strings.Contains(stderr.String(), "new prompt cache") {
		t.Fatalf("the cost of the change was not mentioned: %q", stderr.String())
	}

	// A model that rejects the parameter is refused locally, without a request.
	haiku := newConversation(t, "claude-haiku-4-5", "", 0)
	stderr.Reset()
	haiku.commandEffort("high", &stderr)
	if haiku.cfg.ReasoningEffort != "" || !strings.Contains(stderr.String(), "takes no reasoning effort") {
		t.Fatalf("got %q after %q", haiku.cfg.ReasoningEffort, stderr.String())
	}
}

func TestChat_ModelAndEffortDoNotApplyToAScriptedRun(t *testing.T) {
	_, stderr := chatSession(t, NewScriptedModel(turn(textBlock("x"))), "/model\n/effort\n/help\n/exit\n")

	for _, want := range []string{
		"no model to choose",
		"reasoning effort has no effect",
		"/model   list models",
		"/effort  list reasoning efforts",
	} {
		if !strings.Contains(stderr, want) {
			t.Fatalf("stderr lacks %q:\n%s", want, stderr)
		}
	}
}
