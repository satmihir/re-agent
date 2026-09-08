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
	if code := chat(context.Background(), session, t.TempDir(), strings.NewReader(input), &out, &errs); code != exitOK {
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
