package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// chatSession drives the REPL with canned input and returns what it printed.
func chatSession(t *testing.T, model Model, input string) (stdout, stderr string) {
	t.Helper()
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs, usage: Usage{Known: true}}
	if code := chat(context.Background(), c, newLineReader(strings.NewReader(input), &errs, nil), &out, &errs); code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, errs.String())
	}
	return out.String(), errs.String()
}

type fakeLineReader struct {
	reads []struct {
		line string
		err  error
	}
	prompts       []string
	chosen        int
	chooseErr     error
	chooseCalls   int
	choiceCurrent int
	choiceTitle   string
	choiceConfig  pickerConfig
	choices       []choice
}

func (r *fakeLineReader) Choose(config pickerConfig, options []choice, current int) (int, error) {
	r.chooseCalls++
	r.choiceCurrent = current
	r.choiceTitle, r.choices, r.choiceConfig = config.title, options, config
	return r.chosen, r.chooseErr
}

func (r *fakeLineReader) ReadLine() (string, error) {
	if len(r.reads) == 0 {
		return "", io.EOF
	}
	read := r.reads[0]
	r.reads = r.reads[1:]
	return read.line, read.err
}
func (r *fakeLineReader) SetPrompt(prompt string) { r.prompts = append(r.prompts, prompt) }
func (r *fakeLineReader) SetBandPrefix(string)    {}

func TestChat_SecondInterruptExits(t *testing.T) {
	input := &fakeLineReader{reads: []struct {
		line string
		err  error
	}{{err: errInterrupted}, {err: errInterrupted}}}
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), NewScriptedModel(), NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, traceDir: t.TempDir(), progress: &errs}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d", code)
	}
	if got := strings.Count(errs.String(), "Ctrl-C again"); got != 1 {
		t.Fatalf("warning count %d: %q", got, errs.String())
	}
}

func TestChat_SingleInterruptKeepsTheConversation(t *testing.T) {
	input := &fakeLineReader{reads: []struct {
		line string
		err  error
	}{{err: errInterrupted}, {line: "/exit"}}}
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), NewScriptedModel(), NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, traceDir: t.TempDir(), progress: &errs}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d", code)
	}
	if !strings.Contains(errs.String(), "Ctrl-C again") {
		t.Fatalf("stderr: %q", errs.String())
	}
}

func TestChat_SnapshotOnEveryTurn(t *testing.T) {
	var errs, out bytes.Buffer
	session := NewSession(testConfig(t), NewScriptedModel(turn(textBlock("one")), turn(textBlock("two"))), NewTrace(io.Discard), &errs)
	n := 0
	session.snapshot = func(context.Context) json.RawMessage {
		n++
		return json.RawMessage(fmt.Sprintf(`{"kind":"workspace_state","date":"day-%d"}`, n))
	}
	c := &conversation{session: session, cfg: session.cfg, scripted: session.model, traceDir: t.TempDir(), progress: &errs, usage: Usage{Known: true}}
	if code := chat(context.Background(), c, newLineReader(strings.NewReader("one\ntwo\n/exit\n"), &errs, nil), &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if n != 2 || string(session.history[0].User.Workspace) != `{"kind":"workspace_state","date":"day-1"}` || string(session.history[2].User.Workspace) != `{"kind":"workspace_state","date":"day-2"}` {
		t.Fatalf("snapshots: %+v", session.history)
	}
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
	// The completed-turn summary stays while the duplicate operation recap is hidden.
	if !strings.Contains(stderr, "completed · 2 steps") || !strings.Contains(stderr, "✓ echo") || strings.Contains(stderr, "  ran ") || strings.Contains(stderr, "> ") {
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
	if !strings.Contains(stderr, "protocol_error") || !strings.Contains(stderr, "use /compact or /reset") {
		t.Fatalf("stderr: %q", stderr)
	}
	if !strings.Contains(stderr, "fresh session") {
		t.Fatalf("stderr: %q", stderr)
	}
}

func TestChat_PlanPickerImplementsHere(t *testing.T) {
	var out, errs bytes.Buffer
	model := NewScriptedModel(turn(textBlock("<plan>\n- Change the parser\n</plan>")), turn(textBlock("implemented")))
	cfg := testConfig(t)
	cfg.PlanMode = true
	session := NewSession(cfg, model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: cfg, scripted: model, traceDir: t.TempDir(), progress: &errs, usage: Usage{Known: true}}
	input := &fakeLineReader{reads: []struct {
		line string
		err  error
	}{{line: "plan it"}, {line: "/exit"}}}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if input.chooseCalls != 1 || input.choiceTitle != "Plan ready" || input.choiceCurrent != 2 || input.choiceConfig.shortcuts || input.choiceConfig.cancelLabel != "keep planning" || len(input.choices) != 3 || input.choices[2].label != "Keep planning" {
		t.Fatalf("picker: %+v", input)
	}
	if session.planMode || model.next != 2 || session.Turns() != 2 || len(session.history) != 4 ||
		session.history[0].User.Plan != "on" || session.history[2].User.Plan != "ended" || session.history[2].User.Text != "Implement the plan." {
		t.Fatalf("mode %t, calls %d, history %+v", session.planMode, model.next, session.history)
	}
	if session.history[1].Assistant.Blocks[0].Text != "<plan>\n- Change the parser\n</plan>" || !strings.Contains(errs.String(), "plan mode off\n> Implement the plan.\n") {
		t.Fatalf("history %+v; stderr %q", session.history, errs.String())
	}
}

func TestChat_PlanPickerImplementsFresh(t *testing.T) {
	var out, errs bytes.Buffer
	model := NewScriptedModel(turn(textBlock("<plan>\nold\n</plan>\n<plan>\n- Change the parser\n</plan>")), turn(textBlock("implemented")))
	cfg := testConfig(t)
	cfg.PlanMode = true
	traceDir := t.TempDir()
	session := NewSession(cfg, model, NewTrace(io.Discard), &errs)
	before := session.ID
	c := &conversation{session: session, cfg: cfg, scripted: model, traceDir: traceDir, progress: &errs, usage: Usage{Known: true}}
	input := &fakeLineReader{chosen: 1, reads: []struct {
		line string
		err  error
	}{{line: "plan it"}, {line: "/exit"}}}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	want := "Implement this plan. It was made in an earlier re:agent session, and the workspace may have changed since; check what you rely on.\n\n- Change the parser"
	if c.session != session || session.ID == before || session.planMode || session.Turns() != 1 || model.next != 2 ||
		len(session.history) != 2 || session.history[0].User.Text != want || session.history[0].User.Plan != "" {
		t.Fatalf("id %q (was %q), mode %t, calls %d, history %+v", session.ID, before, session.planMode, model.next, session.history)
	}
	paths, err := filepath.Glob(filepath.Join(traceDir, "*", "events.jsonl"))
	if err != nil || len(paths) != 2 {
		t.Fatalf("old and fresh traces: %q, %v", paths, err)
	}
}

func TestChat_PlanPickerKeepsPlanning(t *testing.T) {
	for _, test := range []struct {
		name  string
		index int
		err   error
	}{{"cancelled", 0, errCancelled}, {"keep planning", 2, nil}} {
		t.Run(test.name, func(t *testing.T) {
			var out, errs bytes.Buffer
			model := NewScriptedModel(turn(textBlock("<plan>\n- Change it\n</plan>")))
			cfg := testConfig(t)
			cfg.PlanMode = true
			session := NewSession(cfg, model, NewTrace(io.Discard), &errs)
			c := &conversation{session: session, cfg: cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
			input := &fakeLineReader{chosen: test.index, chooseErr: test.err, reads: []struct {
				line string
				err  error
			}{{line: "plan it"}}}
			if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
				t.Fatalf("exit %d: %s", code, errs.String())
			}
			if input.chooseCalls != 1 || !session.planMode || session.Turns() != 1 || len(session.history) != 2 || model.next != 1 || !strings.Contains(errs.String(), "kept planning\n") {
				t.Fatalf("mode %t, calls %d, history %+v, stderr %q", session.planMode, model.next, session.history, errs.String())
			}
		})
	}
}

func TestChat_PlanPickerTypedFeedbackCannotImplement(t *testing.T) {
	var out, errs bytes.Buffer
	model := NewScriptedModel(turn(textBlock("<plan>\n- Change step 2\n</plan>")), turn(textBlock("IMPLEMENTATION TURN RAN")))
	cfg := testConfig(t)
	cfg.PlanMode = true
	session := NewSession(cfg, model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	input := terminalInput(&pickerKeys{chunks: [][]byte{[]byte("plan it\r"), []byte("ok but change step 2\r"), []byte("/exit\r")}})
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if !session.planMode || session.Turns() != 1 || model.next != 1 || strings.Contains(out.String(), "IMPLEMENTATION TURN RAN") {
		t.Fatalf("mode %t, turns %d, calls %d, output %q", session.planMode, session.Turns(), model.next, out.String())
	}
	if !strings.Contains(errs.String(), "kept planning") {
		t.Fatalf("stderr: %q", errs.String())
	}
}

func TestChat_NoPickerWithoutAPlan(t *testing.T) {
	for _, test := range []struct {
		name     string
		response ModelResponse
	}{{"no block", turn(textBlock("not yet"))}, {"unclosed", turn(textBlock("<plan>\nstill drafting"))}, {"refused", turn(refusalBlock("<plan>\nno\n</plan>"))}} {
		t.Run(test.name, func(t *testing.T) {
			var out, errs bytes.Buffer
			model := NewScriptedModel(test.response)
			cfg := testConfig(t)
			cfg.PlanMode = true
			session := NewSession(cfg, model, NewTrace(io.Discard), &errs)
			c := &conversation{session: session, cfg: cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
			input := &fakeLineReader{reads: []struct {
				line string
				err  error
			}{{line: "plan it"}}}
			if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
				t.Fatalf("exit %d: %s", code, errs.String())
			}
			if input.chooseCalls != 0 || !session.planMode || model.next != 1 || strings.Contains(errs.String(), "/plan turns plan mode off") {
				t.Fatalf("picker %d, mode %t, calls %d, stderr %q", input.chooseCalls, session.planMode, model.next, errs.String())
			}
		})
	}
}

func TestChat_PlanBlockDoesNotOpenPickerOutsidePlanMode(t *testing.T) {
	var out, errs bytes.Buffer
	model := NewScriptedModel(turn(textBlock("<plan>\n- Change it\n</plan>")))
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	input := &fakeLineReader{reads: []struct {
		line string
		err  error
	}{{line: "question"}}}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if input.chooseCalls != 0 || session.planMode || model.next != 1 || session.Turns() != 1 {
		t.Fatalf("picker %d, mode %t, calls %d, history %+v", input.chooseCalls, session.planMode, model.next, session.history)
	}
}

func TestChat_PlanWithoutATerminalPrintsTheHint(t *testing.T) {
	var out, errs bytes.Buffer
	model := NewScriptedModel(turn(textBlock("<plan>\n- Change it\n</plan>")))
	cfg := testConfig(t)
	cfg.PlanMode = true
	session := NewSession(cfg, model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	input := newLineReader(strings.NewReader("plan it\n"), &errs, nil)
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if !strings.Contains(errs.String(), "/plan turns plan mode off; then ask for the implementation\n") || !session.planMode || model.next != 1 {
		t.Fatalf("mode %t, calls %d, stderr %q", session.planMode, model.next, errs.String())
	}
}

func TestChat_PlanInASentenceTurnsItOn(t *testing.T) {
	var out, errs bytes.Buffer
	model := NewScriptedModel(turn(textBlock("still planning")))
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	input := &fakeLineReader{reads: []struct {
		line string
		err  error
	}{{line: "   can we /plan the retry change?   "}}}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if session.Turns() != 1 || !session.planMode || model.next != 1 || session.history[0].User.Text != "can we /plan the retry change?" || session.history[0].User.Plan != "on" {
		t.Fatalf("history: %+v; mode %t, calls %d", session.history, session.planMode, model.next)
	}
	if strings.Count(errs.String(), "plan mode on: edits and commands are refused until /plan again") != 1 || len(input.prompts) != 2 || input.prompts[0] != "> " || input.prompts[1] != "plan> " {
		t.Fatalf("stderr %q; prompts %q", errs.String(), input.prompts)
	}
}

func TestChat_PlanSentenceBandKeepsOriginalPrompt(t *testing.T) {
	t.Setenv("NO_COLOR", "")
	terminal, err := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	defer terminal.Close()
	text := "can we /plan the retry change?"
	input := terminalInput(strings.NewReader(text + "\r/exit\r"))
	input.styled = true
	var out bytes.Buffer
	model := NewScriptedModel(turn(textBlock("planning")))
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), terminal)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: terminal}
	if code := chat(context.Background(), c, input, &out, terminal); code != exitOK {
		t.Fatalf("exit %d", code)
	}
	band := input.out.(*bytes.Buffer).String()
	if !strings.Contains(band, ansiUserBand+"> "+text+"\x1b[K") || !strings.Contains(band, ansiUserBand+ansiPromptTeal+"plan\x1b[22;39m> /exit\x1b[K") {
		t.Fatalf("the first message should show the old prompt, then the plan prompt: %q", band)
	}
	if !session.planMode || session.history[0].User.Plan != "on" {
		t.Fatalf("mode %t, history %+v", session.planMode, session.history)
	}
}

func TestChat_PlanInPastedMessage(t *testing.T) {
	text := "can we plan this?\nlet's /plan it"
	input := &fakeLineReader{reads: []struct {
		line string
		err  error
	}{{line: text}}}
	var out, errs bytes.Buffer
	model := NewScriptedModel(turn(textBlock("planning")))
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK || !session.planMode || session.history[0].User.Text != text || session.history[0].User.Plan != "on" {
		t.Fatalf("exit %d; history %+v; stderr %q", code, session.history, errs.String())
	}
}

func TestChat_PlanInASentenceWhenAlreadyOn(t *testing.T) {
	model := NewScriptedModel(turn(textBlock("one")), turn(textBlock("two")))
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	input := newLineReader(strings.NewReader("can we /plan this?\ncan we /plan more?\n"), &errs, nil)
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if strings.Count(errs.String(), "plan mode on:") != 1 || model.next != 2 || session.history[2].User.Plan != "on" || session.history[2].User.Text != "can we /plan more?" {
		t.Fatalf("stderr %q; history %+v", errs.String(), session.history)
	}
}

func TestChat_PlanInCodeOrPathDoesNothing(t *testing.T) {
	model := NewScriptedModel(turn(textBlock("one")), turn(textBlock("two")))
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	input := newLineReader(strings.NewReader("see docs/plan.md\nthe `/plan` command\n"), &errs, nil)
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if session.planMode || model.next != 2 || session.history[0].User.Plan != "" || session.history[2].User.Plan != "" || strings.Contains(errs.String(), "plan mode on:") {
		t.Fatalf("stderr %q; history %+v", errs.String(), session.history)
	}
}

func TestChat_PlanInEditedMessage(t *testing.T) {
	editor := filepath.Join(t.TempDir(), "editor")
	if err := os.WriteFile(editor, []byte("#!/bin/sh\nprintf 'can we /plan this from the editor?\\n' > \"$1\"\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("VISUAL", editor)
	stdin, err := os.Open(os.DevNull)
	if err != nil {
		t.Fatal(err)
	}
	defer stdin.Close()
	terminal, err := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	defer terminal.Close()
	var out, errs bytes.Buffer
	model := NewScriptedModel(turn(textBlock("plan")))
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs, stdin: stdin, stdout: terminal, stderr: terminal}
	input := newLineReader(strings.NewReader("/edit\n"), &errs, nil)
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if !session.planMode || model.next != 1 || session.history[0].User.Plan != "on" || session.history[0].User.Text != "can we /plan this from the editor?\n" {
		t.Fatalf("history %+v; mode %t", session.history, session.planMode)
	}
}

func TestChat_ShellLineWithPlanDoesNothing(t *testing.T) {
	var out, errs bytes.Buffer
	cfg := testConfig(t)
	cfg.WorkspacePath = t.TempDir()
	session := NewSession(cfg, NewScriptedModel(), NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: cfg, traceDir: t.TempDir(), progress: &errs}
	input := newLineReader(strings.NewReader("!printf '/plan\\n'\n"), &errs, nil)
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if session.planMode || session.Turns() != 0 || len(session.history) != 1 || session.history[0].Kind != EntryShell || strings.Contains(errs.String(), "plan mode on:") {
		t.Fatalf("stderr %q; history %+v", errs.String(), session.history)
	}
}

func TestChat_PlanSentenceEncodesLikeExplicitMode(t *testing.T) {
	text := "can we /plan the retry change?"
	var bodies [2][2][]byte
	padded := "   " + text + "   "
	for i, inputText := range []string{padded + "\n", "/plan\n" + padded + "\n"} {
		var out, errs bytes.Buffer
		model := NewScriptedModel(turn(textBlock("planning")))
		session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
		session.snapshot = func(context.Context) json.RawMessage { return json.RawMessage(`{"kind":"workspace_state"}`) }
		c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
		if code := chat(context.Background(), c, newLineReader(strings.NewReader(inputText), &errs, nil), &out, &errs); code != exitOK {
			t.Fatalf("exit %d: %s", code, errs.String())
		}
		if session.history[0].User.Plan != "on" || session.history[0].User.Text != text {
			t.Fatalf("user entry: %+v", session.history[0])
		}
		for p, encode := range []func(ModelRequest) ([]byte, error){EncodeOpenAIRequest, EncodeAnthropicRequest} {
			body, err := encode(BuildContext(session.cfg, RequestScope{}, session.history[:1]))
			if err != nil {
				t.Fatal(err)
			}
			bodies[i][p] = body
		}
	}
	for p := range bodies[0] {
		if !bytes.Equal(bodies[0][p], bodies[1][p]) {
			t.Fatalf("provider %d: sentence and explicit mode encoded differently", p)
		}
	}
}

func TestChat_PlanSwitchLinesAndStatusColour(t *testing.T) {
	on := "plan mode on: edits and commands are refused until /plan again"
	if got := planSwitchLine(true, true); got != ansiPromptTeal+on+ansiReset {
		t.Fatalf("styled on line %q", got)
	}
	if got := planSwitchLine(false, true); got != ansiDim+"plan mode off"+ansiReset {
		t.Fatalf("styled off line %q", got)
	}
	if planSwitchLine(true, false) != on || planSwitchLine(false, false) != "plan mode off" || planModeLabel(false) != "plan mode" || planModeLabel(true) != ansiPromptTeal+"plan mode"+ansiReset {
		t.Fatal("plain or styled plan mode changed")
	}
}

func TestChat_PlanPromptIsTeal(t *testing.T) {
	terminal, err := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	defer terminal.Close()
	for _, test := range []struct {
		name, noColor, blocked, want string
	}{
		{"styled", "", "", ansiPromptTeal + "plan" + ansiReset + "> "},
		{"blocked", "", "protocol_error", "(blocked) " + ansiPromptTeal + "plan" + ansiReset + "> "},
		{"NO_COLOR", "1", "", "plan> "},
	} {
		t.Run(test.name, func(t *testing.T) {
			t.Setenv("NO_COLOR", test.noColor)
			var out bytes.Buffer
			session := NewSession(testConfig(t), NewScriptedModel(), NewTrace(io.Discard), terminal)
			session.blocked = test.blocked
			c := &conversation{session: session, cfg: session.cfg, traceDir: t.TempDir(), progress: terminal}
			input := &fakeLineReader{reads: []struct {
				line string
				err  error
			}{{line: "/plan"}}}
			if code := chat(context.Background(), c, input, &out, terminal); code != exitOK || len(input.prompts) != 2 || input.prompts[1] != test.want {
				t.Fatalf("exit %d; prompts %q, want %q", code, input.prompts, test.want)
			}
		})
	}
}

func TestChat_PlanTogglesAndChangesThePrompt(t *testing.T) {
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), NewScriptedModel(), NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, traceDir: t.TempDir(), progress: &errs}
	input := &fakeLineReader{}
	for _, line := range []string{"/plan", "/status", "/plan", "/status"} {
		input.reads = append(input.reads, struct {
			line string
			err  error
		}{line: line})
	}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if got := input.prompts; len(got) != 5 || got[0] != "> " || got[1] != "plan> " || got[2] != "plan> " || got[3] != "> " || got[4] != "> " {
		t.Fatalf("prompts: %q", got)
	}
	if !strings.Contains(errs.String(), "plan mode on: edits and commands are refused until /plan again\n") || !strings.Contains(errs.String(), "plan mode off\n") || !strings.Contains(errs.String(), "mode       read, write, and execute · plan mode") {
		t.Fatalf("stderr: %q", errs.String())
	}
	if session.Turns() != 0 {
		t.Fatalf("commands spent turns: %+v", session.history)
	}
}

func TestChat_PlanReplyKeepsHistoryVerbatim(t *testing.T) {
	var out, errs bytes.Buffer
	model := NewScriptedModel(turn(textBlock("<plan>\nDo this.\n</plan>")))
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	if code := chat(context.Background(), c, newLineReader(strings.NewReader("/plan work\n/exit\n"), &errs, nil), &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if strings.Contains(out.String(), "<plan>") || !strings.Contains(out.String(), "plan\nDo this.") || session.history[1].Assistant.Blocks[0].Text != "<plan>\nDo this.\n</plan>" {
		t.Fatalf("display %q, history %+v", out.String(), session.history)
	}
}

func TestChat_PlanWithTextSendsInPlanMode(t *testing.T) {
	model := NewScriptedModel(turn(textBlock("plan")))
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs}
	if code := chat(context.Background(), c, newLineReader(strings.NewReader("/plan fix the parser\n/exit\n"), &errs, nil), &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if session.Turns() != 1 || session.history[0].User.Plan != "on" || session.history[0].User.Text != "fix the parser" || model.next != 1 {
		t.Fatalf("history: %+v; calls %d", session.history, model.next)
	}
}

func TestChat_PlanSurvivesResetAndModel(t *testing.T) {
	c := newConversation(t, "gpt-6-luna", "low", 0)
	c.session.planMode = true
	c.session.Reset()
	if !c.session.planMode {
		t.Fatal("reset lost plan mode")
	}
	c.switchTo(modelCatalog[1])
	if !c.session.planMode {
		t.Fatal("model switch lost plan mode")
	}
}

func TestChat_ShellCommandWorksInPlanMode(t *testing.T) {
	var out, errs bytes.Buffer
	cfg := testConfig(t)
	cfg.WorkspacePath = t.TempDir()
	session := NewSession(cfg, NewScriptedModel(turn(textBlock("ok"))), NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: cfg, scripted: session.model, traceDir: t.TempDir(), progress: &errs}
	input := newLineReader(strings.NewReader("/plan\n!echo hi\nquestion\n"), &errs, nil)
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if len(session.history) < 2 || session.history[0].Kind != EntryShell || !strings.Contains(session.history[0].Shell.Output, "hi") || session.history[1].User.Plan != "on" {
		t.Fatalf("history: %+v", session.history)
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
	native := openaiProvider
	item := json.RawMessage(`{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}`)
	if cfg.Provider == anthropicName {
		native = anthropicProvider
		item = json.RawMessage(`{"type":"text","text":"answer"}`)
	}
	for i := 0; i < turns; i++ {
		c.session.history = append(c.session.history,
			Entry{Kind: EntryUser, User: &UserTurn{Text: "q"}},
			Entry{Kind: EntryAssistant, Assistant: &ModelResponse{Native: NativeOutput{Provider: native, Items: []json.RawMessage{item}}}})
	}
	return c
}

func TestChat_ProjectInstructionsSurviveSwitchAndReset(t *testing.T) {
	c := newConversation(t, "gpt-6-luna", "low", 1)
	project := "launch-time instructions\n"
	c.cfg.ProjectInstructions = &project
	c.session.cfg.ProjectInstructions = &project
	c.session.Reset()
	if got := instructions(c.session.cfg); !strings.HasSuffix(got, "# Project instructions (AGENTS.md)\n\n"+project) {
		t.Fatalf("reset lost project instructions: %q", got)
	}
	c.switchTo(modelCatalog[1])
	if got := instructions(c.session.cfg); !strings.HasSuffix(got, "# Project instructions (AGENTS.md)\n\n"+project) {
		t.Fatalf("model switch lost project instructions: %q", got)
	}
}

func TestChat_ModelPickerSwitches(t *testing.T) {
	c := newConversation(t, "gpt-6-luna", "low", 1)
	before := c.session
	input := &fakeLineReader{chosen: 1}
	input.reads = append(input.reads, struct {
		line string
		err  error
	}{line: "/model"})
	var out, errs bytes.Buffer
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if input.chooseCalls != 1 || !input.choiceConfig.shortcuts || input.choiceCurrent != 0 || c.cfg.Model != modelCatalog[1].ID || c.session == before || c.session.Turns() != 0 || out.Len() != 0 {
		t.Fatalf("picker calls %d, current %d, model %s, output %q", input.chooseCalls, input.choiceCurrent, c.cfg.Model, out.String())
	}
	if len(c.session.history) != 1 || c.session.history[0].Summary.Text != "x" || !strings.Contains(errs.String(), "switched to gpt-6.1-sol") || !strings.Contains(errs.String(), "carried the conversation over") {
		t.Fatalf("stderr: %s", errs.String())
	}
}

func TestChat_ModelPickerCancelKeepsTheModel(t *testing.T) {
	c := newConversation(t, "gpt-6-luna", "low", 2)
	before := c.session
	input := &fakeLineReader{chooseErr: errCancelled}
	input.reads = append(input.reads, struct {
		line string
		err  error
	}{line: "/model"})
	var out, errs bytes.Buffer
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d", code)
	}
	if c.session != before || c.session.Turns() != 2 || strings.TrimSpace(errs.String()) != "kept gpt-6-luna" {
		t.Fatalf("session changed or wrong result: %s", errs.String())
	}
}

func TestChat_ModelWithoutATerminalPrintsTheList(t *testing.T) {
	model := &requestRecorder{ScriptedModel: NewScriptedModel(turn(textBlock("reply")))}
	c := newConversation(t, "gpt-6-luna", "low", 0)
	c.session = NewSession(c.cfg, model, NewTrace(io.Discard), io.Discard)
	var out, errs bytes.Buffer
	input := newLineReader(strings.NewReader("/model\n2\n"), &errs, nil)
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if len(model.requests) != 1 || model.requests[0].History[0].User.Text != "2" || c.cfg.Model != "gpt-6-luna" || !strings.Contains(errs.String(), "/model <number or name> switches\n") {
		t.Fatalf("requests %+v; stderr %s", model.requests, errs.String())
	}
}

func TestChat_EffortPickerKeepsSession(t *testing.T) {
	c := newConversation(t, "gpt-6-luna", "low", 2)
	before := c.session
	input := &fakeLineReader{chosen: 3}
	input.reads = append(input.reads, struct {
		line string
		err  error
	}{line: "/effort"})
	var out, errs bytes.Buffer
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d", code)
	}
	if input.choiceCurrent != 1 || c.cfg.ReasoningEffort != "high" || c.session != before || c.session.Turns() != 2 || !strings.Contains(errs.String(), "reasoning effort is now high") {
		t.Fatalf("effort %q, current %d, stderr %s", c.cfg.ReasoningEffort, input.choiceCurrent, errs.String())
	}
}

func TestChat_EffortPickerCancelWithUnsetEffort(t *testing.T) {
	c := newConversation(t, "gpt-6-luna", "", 1)
	before := c.session
	input := &fakeLineReader{chooseErr: errCancelled}
	input.reads = append(input.reads, struct {
		line string
		err  error
	}{line: "/effort"})
	var out, errs bytes.Buffer
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if input.choiceCurrent != 1 || c.cfg.ReasoningEffort != "" || c.session != before || c.session.Turns() != 1 || strings.TrimSpace(errs.String()) != "kept provider default" {
		t.Fatalf("cursor %d, effort %q, session %p/%p, stderr %q", input.choiceCurrent, c.cfg.ReasoningEffort, c.session, before, errs.String())
	}
}

// An explicit fresh switch never sends the old history to the new model.
func TestConversation_SwitchingModelStartsAFreshSession(t *testing.T) {
	c := newConversation(t, "gpt-5.6-luna", "xhigh", 3)
	before := c.session

	var stderr bytes.Buffer
	c.commandModel(context.Background(), "claude-haiku-4-5 fresh", nil, &stderr)

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
	if !strings.Contains(stderr.String(), "conversation discarded") {
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
			"claude-sonnet-5-5", "needs ANTHROPIC_API_KEY",
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
			conv.commandModel(context.Background(), c.choice, nil, &stderr)

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
	c := newConversation(t, "claude-sonnet-5-5", "low", 2)
	before := c.session

	var stderr bytes.Buffer
	c.commandEffort("xhigh", nil, &stderr)

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
	haiku.commandEffort("high", nil, &stderr)
	if haiku.cfg.ReasoningEffort != "" || !strings.Contains(stderr.String(), "takes no reasoning effort") {
		t.Fatalf("got %q after %q", haiku.cfg.ReasoningEffort, stderr.String())
	}
}

func TestChat_ModelAndEffortDoNotApplyToAScriptedRun(t *testing.T) {
	_, stderr := chatSession(t, NewScriptedModel(turn(textBlock("x"))), "/model\n/effort\n/help\n/exit\n")

	for _, want := range []string{
		"no model to choose",
		"reasoning effort has no effect",
		"/model    [model] [fresh]    choose a model; carry a summary, or discard with fresh",
		"/effort   [number or name]   choose reasoning effort (picker on a terminal)",
	} {
		if !strings.Contains(stderr, want) {
			t.Fatalf("stderr lacks %q:\n%s", want, stderr)
		}
	}
}

func TestChat_StatusReportsSessionState(t *testing.T) {
	model := NewScriptedModel(ModelResponse{
		Blocks: []OutputBlock{textBlock("done")},
		Usage:  Usage{Known: true, InputTokens: 1200, CachedInputTokens: 800, OutputTokens: 30},
	})
	_, stderr := chatSession(t, model, "question\n/status\n/reset\n/status\n/exit\n")
	for _, want := range []string{
		"model      ", "mode       read, write, and execute", "1 turn", "1.2k in (800 cached) · 30 out", "last trace ",
		"session    0 turns · 0 in (0 cached) · 0 out",
	} {
		if !strings.Contains(stderr, want) {
			t.Fatalf("status lacks %q:\n%s", want, stderr)
		}
	}

	_, blocked := chatSession(t, NewScriptedModel(
		turn(callBlock("call_1", "echo", `{"text":"hi"}`), callBlock("call_1", "echo", `{"text":"hi"}`))),
		"question\n/status\n/exit\n")
	if !strings.Contains(blocked, "blocked by protocol_error; /compact or /reset to continue") {
		t.Fatalf("blocked status: %s", blocked)
	}
}

func TestChat_OverflowOnlyOffersReset(t *testing.T) {
	var out, errs bytes.Buffer
	cfg := testConfig(t)
	cfg.Model = "gpt-6-luna"
	model := &failingThen{
		err:  &ModelError{Status: StatusLimitExceeded, Message: "the conversation no longer fits the model's context window", Usage: Usage{Known: true, InputTokens: 700_000}},
		next: NewScriptedModel(),
	}
	s := NewSession(cfg, model, NewTrace(io.Discard), &errs)
	c := &conversation{session: s, cfg: cfg, traceDir: t.TempDir(), usage: Usage{Known: true}}
	input := newLineReader(strings.NewReader("first\n/status\nnext\n/exit\n"), &errs, nil)
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatal(code)
	}
	text := errs.String()
	if strings.Count(text, "/reset to continue; /compact works before the window fills (watch the 60% warning)") != 3 ||
		strings.Contains(text, "/compact or /reset") || strings.Contains(text, "context 67% of the window; /compact") || s.blocked != string(StatusLimitExceeded) {
		t.Fatalf("overflow guidance: %s", text)
	}
}

func TestChat_WindowMeterInStatusAndContext(t *testing.T) {
	c := newConversation(t, "gpt-6-luna", "low", 0)
	c.session.lastRequest = Usage{Known: true, InputTokens: 152_300, CachedInputTokens: 90_000}
	var status, context bytes.Buffer
	c.commandStatus(&status)
	c.commandContext(&context)
	want := "last request  152.3k tokens · 15% of the 1.05M window"
	if !strings.Contains(status.String(), want) || !strings.Contains(context.String(), want) {
		t.Fatalf("status: %s\ncontext: %s", status.String(), context.String())
	}
	c.session.Reset()
	status.Reset()
	c.commandStatus(&status)
	if strings.Contains(status.String(), "last request") {
		t.Fatalf("reset left a meter: %s", status.String())
	}
	c.session.lastRequest = Usage{Known: true, InputTokens: 152_300}
	info, _ := findModel("claude-haiku-4-5")
	c.switchTo(info)
	status.Reset()
	c.commandStatus(&status)
	if strings.Contains(status.String(), "last request") {
		t.Fatalf("model switch left a meter: %s", status.String())
	}
}

func TestChat_WarnsFromLastRequestNotTurnTotal(t *testing.T) {
	for _, test := range []struct {
		name  string
		last  int64
		warns bool
	}{
		{"below", 119_999, false},
		{"at", 120_000, true},
	} {
		t.Run(test.name, func(t *testing.T) {
			c := newConversation(t, "claude-haiku-4-5", "", 0)
			first := turn(callBlock("call_1", "echo", `{"text":"hi"}`))
			first.Usage = Usage{Known: true, InputTokens: 190_000}
			second := turn(textBlock("done"))
			second.Usage = Usage{Known: true, InputTokens: test.last}
			c.session.model = NewScriptedModel(first, second)
			var stdout, stderr bytes.Buffer
			c.session.display = NewDisplay(&stderr)
			c.runTurn(context.Background(), "task", nil, &stdout, &stderr)
			if got := strings.Contains(stderr.String(), "/reset starts over"); got != test.warns {
				t.Fatalf("warning %t, want %t: %s", got, test.warns, stderr.String())
			}
			if c.session.lastRequest.InputTokens != test.last || c.usage.InputTokens != 190_000+test.last {
				t.Fatalf("last %+v, total %+v", c.session.lastRequest, c.usage)
			}
		})
	}
}

func TestChat_StatusFormatsModel(t *testing.T) {
	c := newConversation(t, "claude-haiku-4-5", "", 0)
	var stderr bytes.Buffer
	c.commandStatus(&stderr)
	if !strings.Contains(stderr.String(), "model      claude-haiku-4-5 (anthropic), default effort") {
		t.Fatalf("status: %s", stderr.String())
	}

	c.cfg.Provider = "scripted"
	stderr.Reset()
	c.commandStatus(&stderr)
	if !strings.Contains(stderr.String(), "model      scripted\n") || strings.Contains(stderr.String(), "effort") {
		t.Fatalf("scripted status: %s", stderr.String())
	}
}

func TestChat_UnknownCommandSuggests(t *testing.T) {
	_, stderr := chatSession(t, NewScriptedModel(), "/mdoel\n/edi\n/zz\n/exit\n")
	for _, want := range []string{
		"unknown command /mdoel; did you mean /model? /help lists commands.\n",
		"unknown command /edi; did you mean /edit? /help lists commands.\n",
		"unknown command /zz; /help lists commands.\n",
	} {
		if !strings.Contains(stderr, want) {
			t.Fatalf("missing %q: %s", want, stderr)
		}
	}
}

func TestChat_BlockedPrompt(t *testing.T) {
	input := &fakeLineReader{reads: []struct {
		line string
		err  error
	}{
		{line: "question"},
		{line: "/exit"},
	}}
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), NewScriptedModel(
		turn(callBlock("call_1", "echo", `{"text":"hi"}`), callBlock("call_1", "echo", `{"text":"hi"}`))), NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, traceDir: t.TempDir(), progress: &errs, usage: Usage{Known: true}}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d", code)
	}
	if len(input.prompts) < 2 || input.prompts[0] != "> " || input.prompts[1] != "(blocked) > " {
		t.Fatalf("prompts: %#v", input.prompts)
	}
}

func TestChat_HelpListsEveryCommand(t *testing.T) {
	_, stderr := chatSession(t, NewScriptedModel(), "/help\n/exit\n")
	for _, command := range chatCommands {
		if !strings.Contains(stderr, command.name) {
			t.Fatalf("help lacks %s:\n%s", command.name, stderr)
		}
	}
}

func TestChat_EditNeedsTerminal(t *testing.T) {
	model := NewScriptedModel(turn(textBlock("unused")))
	_, stderr := chatSession(t, model, "/edit\n/exit\n")
	if !strings.Contains(stderr, "/edit needs a terminal") {
		t.Fatalf("stderr: %s", stderr)
	}
	if model.next != 0 {
		t.Fatalf("the model was called %d times", model.next)
	}
}

func TestComposeInEditor(t *testing.T) {
	stdin, err := os.Open(os.DevNull)
	if err != nil {
		t.Fatal(err)
	}
	defer stdin.Close()
	stdout, err := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	defer stdout.Close()
	stderr, err := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	defer stderr.Close()

	write := t.TempDir() + "/write"
	if err := os.WriteFile(write, []byte("#!/bin/sh\nprintf 'known text\\n' > \"$1\"\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	text, err := composeInEditor([]string{write}, stdin, stdout, stderr)
	if err != nil || text != "known text\n" {
		t.Fatalf("text %q, err %v", text, err)
	}

	empty := t.TempDir() + "/empty"
	if err := os.WriteFile(empty, []byte("#!/bin/sh\n: > \"$1\"\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	text, err = composeInEditor([]string{empty}, stdin, stdout, stderr)
	if err != nil || text != "" {
		t.Fatalf("text %q, err %v", text, err)
	}

	fail := t.TempDir() + "/fail"
	if err := os.WriteFile(fail, []byte("#!/bin/sh\nexit 1\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	if _, err := composeInEditor([]string{fail}, stdin, stdout, stderr); err == nil {
		t.Fatal("failing editor succeeded")
	}
}

// requestRecorder keeps every request a scripted model was asked to answer.
type requestRecorder struct {
	*ScriptedModel
	requests []ModelRequest
}

func (m *requestRecorder) Generate(ctx context.Context, req ModelRequest) (ModelResponse, error) {
	m.requests = append(m.requests, req)
	return m.ScriptedModel.Generate(ctx, req)
}

func TestChat_BangRunsLocallyAndSpendsNothing(t *testing.T) {
	model := NewScriptedModel()
	stdout, stderr := chatSession(t, model, "!echo hi\n!printf partial\n")

	// Output that stops mid-line is ended, so nothing after it shares its row.
	if stdout != "hi\npartial\n" {
		t.Fatalf("stdout: %q", stdout)
	}
	if !strings.Contains(stderr, "exit 0 · ") || !strings.Contains(stderr, "added to the conversation") {
		t.Fatalf("stderr: %q", stderr)
	}
	if model.next != 0 {
		t.Fatalf("the model was called %d times", model.next)
	}
}

func TestChat_BangAloneExplainsUsage(t *testing.T) {
	stdout, stderr := chatSession(t, NewScriptedModel(), "!\n!   \n")
	if stdout != "" || strings.Count(stderr, "!COMMAND runs COMMAND") != 2 {
		t.Fatalf("stdout %q, stderr %q", stdout, stderr)
	}
}

// A paste is one submission of several lines; one that happens to start with
// ! is still a message, never a script.
func TestChat_MultiLineBangIsAMessage(t *testing.T) {
	input := &fakeLineReader{reads: []struct {
		line string
		err  error
	}{{line: "![diagram](a.png)\nwhat does this show?"}}}
	model := &requestRecorder{ScriptedModel: NewScriptedModel(turn(textBlock("a diagram")))}
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, scripted: model, traceDir: t.TempDir(), progress: &errs, usage: Usage{Known: true}}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}
	if len(model.requests) != 1 || model.requests[0].History[0].Kind != EntryUser {
		t.Fatalf("requests: %+v", model.requests)
	}
}

func TestChat_ShellOutputReachesTheNextRequest(t *testing.T) {
	model := &requestRecorder{ScriptedModel: NewScriptedModel(turn(textBlock("it printed a marker")))}
	var out, errs bytes.Buffer
	cfg := testConfig(t)
	cfg.WorkspacePath = t.TempDir()
	session := NewSession(cfg, model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: cfg, scripted: model, traceDir: t.TempDir(), progress: &errs, usage: Usage{Known: true}}
	input := newLineReader(strings.NewReader("!echo marker-4417; exit 2\nwhat did that print?\n"), &errs, nil)
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d: %s", code, errs.String())
	}

	if len(model.requests) != 1 {
		t.Fatalf("got %d requests", len(model.requests))
	}
	history := model.requests[0].History
	if len(history) != 2 || history[0].Kind != EntryShell || history[1].Kind != EntryUser {
		t.Fatalf("history: %+v", history)
	}
	if shell := history[0].Shell; shell.Output != "marker-4417\n" || *shell.ExitCode != 2 {
		t.Fatalf("shell: %+v", shell)
	}
	// A command is context, not a turn.
	if session.Turns() != 1 || !strings.Contains(errs.String(), "exit 2 · ") {
		t.Fatalf("turns %d, stderr %q", session.Turns(), errs.String())
	}
}

// A model request that failed appended nothing, so the conversation goes on
// without a /reset (v0 §10 amendment of 2026-09-26).
func TestChat_ProviderErrorDoesNotBlock(t *testing.T) {
	input := &fakeLineReader{reads: []struct {
		line string
		err  error
	}{{line: "first"}, {line: "second"}}}
	model := &failingThen{
		err:  &ModelError{Status: StatusProviderError, Message: "provider reported a stream error (server_is_overloaded): try later"},
		next: NewScriptedModel(turn(textBlock("recovered"))),
	}
	var out, errs bytes.Buffer
	session := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	c := &conversation{session: session, cfg: session.cfg, traceDir: t.TempDir(), progress: &errs, usage: Usage{Known: true}}
	if code := chat(context.Background(), c, input, &out, &errs); code != exitOK {
		t.Fatalf("exit %d", code)
	}
	if out.String() != "recovered\n" {
		t.Fatalf("stdout %q, stderr %q", out.String(), errs.String())
	}
	if !strings.Contains(errs.String(), "server_is_overloaded") || !strings.Contains(errs.String(), "your next message continues") {
		t.Fatalf("stderr: %q", errs.String())
	}
	for _, prompt := range input.prompts {
		if prompt != "> " {
			t.Fatalf("prompts: %#v", input.prompts)
		}
	}
}

func TestCompact_ScriptedAndEmptySessionsRefuse(t *testing.T) {
	for _, scripted := range []bool{true, false} {
		model := &compactModel{replies: []ModelResponse{turn(textBlock("unused"))}}
		var out, errs bytes.Buffer
		s := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
		if scripted {
			s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "hi"}}}
		}
		dir := t.TempDir()
		c := &conversation{session: s, cfg: s.cfg, traceDir: dir, usage: Usage{Known: true}}
		if scripted {
			c.scripted = model
		}
		if code := chat(context.Background(), c, newLineReader(strings.NewReader("/compact\n/trace\n/exit\n"), &errs, nil), &out, &errs); code != exitOK {
			t.Fatal(code)
		}
		want := "nothing to compact"
		if scripted {
			want = "--scripted"
		}
		files, _ := os.ReadDir(dir)
		if !strings.Contains(errs.String(), want) || !strings.Contains(errs.String(), "no run has been recorded") || len(files) != 0 || len(model.requests) != 0 || out.Len() != 0 {
			t.Fatalf("out %q err %q files %v", out.String(), errs.String(), files)
		}
	}
}

func TestChat_CompactVerboseFocusAndTrace(t *testing.T) {
	for _, test := range []struct {
		argument, focus string
		verbose         bool
	}{
		{"-v   keep tests", "keep tests", true},
		{"keep -v verbatim", "keep -v verbatim", false},
	} {
		t.Run(test.argument, func(t *testing.T) {
			response := turn(textBlock("  Summary \x1b[31mtest  "))
			response.Usage = Usage{Known: true, InputTokens: 123, OutputTokens: 5}
			model := &compactModel{replies: []ModelResponse{response}}
			var out, errs bytes.Buffer
			s := compactSession(t, model)
			s.display = NewDisplay(&errs)
			s.cfg.Provider = openaiName
			c := &conversation{session: s, cfg: s.cfg, traceDir: t.TempDir(), usage: Usage{Known: true}}
			input := "/compact " + test.argument + "\n/status\n/context\n/trace\n/exit\n"
			if code := chat(context.Background(), c, newLineReader(strings.NewReader(input), &errs, nil), &out, &errs); code != exitOK {
				t.Fatal(code)
			}
			if !strings.HasSuffix(model.requests[0].History[2].User.Text, "\n\nFocus: "+test.focus) || c.usage.InputTokens != 123 || s.lastRequest.InputTokens != 123 || !strings.Contains(errs.String(), "history JSON") || !strings.Contains(errs.String(), "conversation summary") || !strings.Contains(errs.String(), s.LastTrace()) || !strings.Contains(errs.String(), "history    /compact") {
				t.Fatalf("out %q err %q", out.String(), errs.String())
			}
			if test.verbose && (out.String() != "Summary \\x1b[31mtest\n" || strings.Contains(out.String(), "\x1b")) {
				t.Fatalf("stdout %q", out.String())
			}
			if !test.verbose && out.Len() != 0 {
				t.Fatalf("stdout %q", out.String())
			}
		})
	}
}

func TestChat_CompactThenPlanOffCarriesEndedMarker(t *testing.T) {
	model := &compactModel{replies: []ModelResponse{turn(textBlock("handoff")), turn(textBlock("done"))}}
	var out, errs bytes.Buffer
	s := NewSession(testConfig(t), model, NewTrace(io.Discard), &errs)
	s.planMode = true
	s.history = []Entry{{Kind: EntryUser, User: &UserTurn{Text: "plan", Plan: "on"}}}
	c := &conversation{session: s, cfg: s.cfg, traceDir: t.TempDir(), usage: Usage{Known: true}}
	if code := chat(context.Background(), c, newLineReader(strings.NewReader("/compact\n/plan\nimplement\n/exit\n"), &errs, nil), &out, &errs); code != exitOK {
		t.Fatal(code)
	}
	if len(model.requests) != 2 || len(model.requests[1].History) != 2 || model.requests[1].History[1].User.Plan != "ended" || s.planMode {
		t.Fatalf("requests: %+v, stderr: %s", model.requests, errs.String())
	}
}

func TestChat_FailedCompactDoesNotClaimSuccess(t *testing.T) {
	model := &compactModel{err: fmt.Errorf("provider down")}
	var out, errs bytes.Buffer
	s := compactSession(t, model)
	s.blocked = "protocol_error"
	c := &conversation{session: s, cfg: s.cfg, traceDir: t.TempDir(), usage: Usage{Known: true}}
	if code := chat(context.Background(), c, newLineReader(strings.NewReader("/status\n/compact\n/status\n/trace\nnext\n/exit\n"), &errs, nil), &out, &errs); code != exitOK {
		t.Fatal(code)
	}
	if !strings.Contains(errs.String(), "compaction failed: provider down") || strings.Contains(errs.String(), "compacted:") || !strings.Contains(errs.String(), "blocked by protocol_error; /compact or /reset") || !strings.Contains(errs.String(), s.LastTrace()) || s.blocked != "protocol_error" || len(model.requests) != 1 {
		t.Fatalf("stderr: %q", errs.String())
	}
}

func TestConversation_ModelSwitchCarriesOnlySummary(t *testing.T) {
	for _, destination := range []string{"gpt-6.1-sol", "claude-haiku-4-5"} {
		t.Run(destination, func(t *testing.T) {
			c := newConversation(t, "gpt-6-luna", "low", 1)
			old := c.session
			old.history[0].User.Plan = "on"
			old.history[1].Assistant.Native = NativeOutput{Provider: openaiProvider, Items: []json.RawMessage{json.RawMessage(`{"type":"message","role":"assistant","content":[{"type":"output_text","text":"old native text"}]}`)}}
			old.blocked, old.planMode = "protocol_error", true
			old.seenCalls["old-call"] = true
			old.lastRequest = Usage{Known: true, InputTokens: 99}
			old.snapshot = func(context.Context) json.RawMessage { return json.RawMessage(`{"kind":"workspace_state"}`) }
			project := "keep launch instructions"
			c.cfg.ProjectInstructions, old.cfg.ProjectInstructions = &project, &project
			reply := turn(textBlock("handoff"))
			reply.Usage = Usage{Known: true, InputTokens: 123, CachedInputTokens: 80, OutputTokens: 7}
			model := &compactModel{replies: []ModelResponse{reply}}
			old.model = model
			c.usage = Usage{Known: true, InputTokens: 100}
			var stderr bytes.Buffer
			c.commandModel(context.Background(), destination, nil, &stderr)
			if len(model.requests) != 1 || model.requests[0].Model != "gpt-6-luna" || model.requests[0].ReasoningEffort != "low" || len(model.requests[0].History) != 3 || model.requests[0].History[2].User.Text != compactPrompt || len(model.requests[0].History[2].User.Workspace) != 0 {
				t.Fatalf("compaction requests: %+v", model.requests)
			}
			s := c.session
			if s.ID == old.ID || len(s.history) != 1 || s.history[0].Kind != EntrySummary || *s.history[0].Summary != (Summary{Text: "handoff", ReplacedEntries: 2, Model: "gpt-6-luna"}) || s.Turns() != 0 || len(s.seenCalls) != 0 || s.blocked != "" || s.lastRequest != (Usage{}) || c.usage != reply.Usage || !s.planMode || s.compactedPlan != "on" || s.snapshot == nil || s.cfg.ProjectInstructions != &project {
				t.Fatalf("replacement: %+v, usage %+v", s, c.usage)
			}
			if !strings.Contains(stderr.String(), "carried the conversation over as a 7 B summary") || strings.Contains(stderr.String(), "compacted:") || s.LastTrace() == "" || s.LastTrace() != old.LastTrace() {
				t.Fatalf("switch: %s", stderr.String())
			}
			events := readEvents(t, s.LastTrace())
			if events[0].Type != "compaction.requested" || events[1].Type != "compaction.finished" {
				t.Fatalf("events: %+v", events)
			}
			// Both encoders must accept the portable handoff without old native items.
			next := &requestRecorder{ScriptedModel: NewScriptedModel(turn(textBlock("done")))}
			s.model, s.planMode = next, false
			if _, err := s.Turn(context.Background(), "continue", NewID(), filepath.Join(t.TempDir(), "next.jsonl")); err != nil {
				t.Fatal(err)
			}
			req := next.requests[0]
			if len(req.History) != 2 || req.History[0].Summary.Text != "handoff" || req.History[1].User.Plan != "ended" {
				t.Fatalf("next request: %+v", req)
			}
			for _, encode := range []func(ModelRequest) ([]byte, error){EncodeOpenAIRequest, EncodeAnthropicRequest} {
				body, err := encode(req)
				if err != nil || !bytes.Contains(body, []byte(summaryPreamble)) || bytes.Contains(body, []byte("old native text")) {
					t.Fatalf("body %s, error %v", body, err)
				}
			}
			t.Logf("compaction final message: %s", model.requests[0].History[2].User.Text)
			t.Logf("next request history: %s", mustJSON(t, req.History))
		})
	}
}

func TestConversation_ModelSwitchNonTurnHistory(t *testing.T) {
	for _, entry := range []Entry{
		{Kind: EntrySummary, Summary: &Summary{Text: "previous handoff", Model: "gpt-6-luna"}},
		{Kind: EntryShell, Shell: &ShellCommand{Command: "pwd", Output: "workspace"}},
	} {
		t.Run(string(entry.Kind), func(t *testing.T) {
			c := newConversation(t, "gpt-6-luna", "low", 0)
			c.session.history = []Entry{entry}
			model := &compactModel{replies: []ModelResponse{turn(textBlock("handoff"))}}
			c.session.model = model
			c.commandModel(context.Background(), "2", nil, io.Discard)
			if len(model.requests) != 1 || len(c.session.history) != 1 || c.session.history[0].Summary.Text != "handoff" {
				t.Fatalf("requests %+v, history %+v", model.requests, c.session.history)
			}
		})
	}
}

func TestConversation_ModelSwitchFreshAndEmptySkipCompaction(t *testing.T) {
	for _, turns := range []int{0, 1} {
		c := newConversation(t, "gpt-6-luna", "low", turns)
		model := &compactModel{}
		c.session.model = model
		c.session.compactedPlan = "on"
		argument := "2"
		if turns > 0 {
			argument += " fresh"
		}
		c.commandModel(context.Background(), argument, nil, io.Discard)
		if len(model.requests) != 0 || len(c.session.history) != 0 || c.session.compactedPlan != "" || c.session.LastTrace() != "" {
			t.Fatalf("session %+v", c.session)
		}
	}
}

func TestConversation_ModelSwitchFailureStillSwitches(t *testing.T) {
	for _, test := range []struct {
		name   string
		reply  ModelResponse
		err    error
		reason string
	}{
		{"provider", ModelResponse{}, fmt.Errorf("provider down\x1b"), "provider down\\x1b"},

		{"empty", turn(textBlock("  ")), nil, "empty"},
		{"tool", turn(callBlock("new", "counter", `{}`)), nil, "tool call"},
		{"limit", ModelResponse{}, nil, "use /reset"},
	} {
		t.Run(test.name, func(t *testing.T) {
			c := newConversation(t, "gpt-6-luna", "low", 1)
			old := c.session
			old.compactedPlan, old.blocked = "on", "protocol_error"
			model := &compactModel{replies: []ModelResponse{test.reply}, err: test.err}
			runs := 0
			cfg := testConfig(t, countingTool{runs: &runs})
			c.cfg.Registry, old.cfg.Registry = cfg.Registry, cfg.Registry
			old.model = model
			if test.name == "limit" {
				old.history[0].User.Text = strings.Repeat("x", MaxRequestBytes)
			}
			var stderr bytes.Buffer
			c.commandModel(context.Background(), "2", nil, &stderr)
			if c.session.ID == old.ID || c.cfg.Model != modelCatalog[1].ID || len(c.session.history) != 0 || c.session.compactedPlan != "" || c.session.blocked != "" || runs != 0 || c.session.LastTrace() == "" || c.session.LastTrace() != old.LastTrace() {
				t.Fatalf("session %+v, runs %d", c.session, runs)
			}
			if !strings.Contains(stderr.String(), "fresh session, conversation not carried over:") || !strings.Contains(stderr.String(), test.reason) || strings.Contains(stderr.String(), "\x1b") {
				t.Fatalf("stderr: %s", stderr.String())
			}
			data, err := os.ReadFile(c.session.LastTrace())
			if err != nil {
				t.Fatal(err)
			}
			lines := bytes.Split(bytes.TrimSpace(data), []byte("\n"))
			var finished event
			if len(lines) != 2 || json.Unmarshal(lines[1], &finished) != nil || finished.Type != "compaction.failed" {
				t.Fatal("missing compaction failure event")
			}
		})
	}
}

func TestConversation_ModelSwitchArgumentValidation(t *testing.T) {
	for _, argument := range []string{"2 wrong", "2 fresh extra", "gpt-6-luna fresh", "fresh"} {
		c := newConversation(t, "gpt-6-luna", "low", 1)
		before := c.session
		model := &compactModel{}
		before.model = model
		var stderr bytes.Buffer
		c.commandModel(context.Background(), argument, nil, &stderr)
		if c.session != before || len(model.requests) != 0 || stderr.Len() == 0 {
			t.Fatalf("argument %q: %s", argument, stderr.String())
		}
	}
}

func TestConversation_ModelSwitchFirstAdapterRequestHasSummary(t *testing.T) {
	for _, source := range []string{"gpt-6-luna", "claude-haiku-4-5"} {
		t.Run(source, func(t *testing.T) {
			api := newFakeAPI(t, okReply(streamedTextReply()))
			c := newConversation(t, source, "", 1)
			c.proxy = apiProxy{provider: openaiName, endpoint: api.server.URL}
			oldModel := &compactModel{replies: []ModelResponse{turn(textBlock("portable handoff"))}}
			c.session.model = oldModel
			c.commandModel(context.Background(), "gpt-6.1-sol", nil, io.Discard)
			result, err := c.session.Turn(context.Background(), "continue", NewID(), filepath.Join(t.TempDir(), "next.jsonl"))
			if err != nil || result.Status != StatusCompleted {
				t.Fatalf("result %+v, error %v", result, err)
			}
			bodies := api.received()
			if len(bodies) != 1 {
				t.Fatalf("requests: %d", len(bodies))
			}
			var body struct {
				Input []struct {
					Role    string `json:"role"`
					Content []struct {
						Text string `json:"text"`
					} `json:"content"`
				} `json:"input"`
			}
			if err := json.Unmarshal(bodies[0], &body); err != nil {
				t.Fatal(err)
			}
			if len(body.Input) != 2 || body.Input[0].Role != "user" || len(body.Input[0].Content) != 1 || body.Input[0].Content[0].Text != summaryPreamble+"\nportable handoff" || body.Input[1].Content[0].Text != "continue" || bytes.Contains(bodies[0], []byte("answer")) {
				t.Fatalf("body: %s", bodies[0])
			}
		})
	}
}

func TestConversation_CancelledModelSwitchKeepsConversation(t *testing.T) {
	for _, blocked := range []string{"", "protocol_error"} {
		t.Run("blocked="+blocked, func(t *testing.T) {
			c := newConversation(t, "gpt-6-luna", "low", 1)
			s := c.session
			s.planMode, s.blocked, s.compactedPlan = true, blocked, "on"
			s.seenCalls["old-call"] = true
			cfg, history := c.cfg, string(mustJSON(t, s.history))
			usage := Usage{Known: true, InputTokens: 100, CachedInputTokens: 50, OutputTokens: 9}
			c.usage = usage
			s.model = &compactModel{err: &ModelError{Status: StatusCancelled, Message: "cancelled during a model request", Usage: Usage{Known: true, InputTokens: 12}}}
			var stderr bytes.Buffer
			c.commandModel(context.Background(), "2", nil, &stderr)
			if c.cfg.Model != cfg.Model || c.cfg.Provider != cfg.Provider || c.cfg.ReasoningEffort != cfg.ReasoningEffort || c.session != s || string(mustJSON(t, s.history)) != history || !s.planMode || s.blocked != blocked || s.compactedPlan != "on" || !s.seenCalls["old-call"] || c.usage != usage {
				t.Fatalf("cancelled switch changed conversation: %+v, usage %+v", c.session, c.usage)
			}
			if stderr.String() != "kept gpt-6-luna; switch cancelled\n" {
				t.Fatalf("stderr: %s", stderr.String())
			}
			if s.LastTrace() == "" {
				t.Fatal("missing cancellation trace")
			}
			events := readEvents(t, s.LastTrace())
			if len(events) != 2 || events[1].Type != "compaction.failed" {
				t.Fatalf("events: %+v", events)
			}
		})
	}
}

func TestConversation_FailedModelSwitchRetainsHandoffUsage(t *testing.T) {
	c := newConversation(t, "gpt-6-luna", "low", 1)
	usage := Usage{Known: true, InputTokens: 123, CachedInputTokens: 80, OutputTokens: 7}
	c.usage = Usage{Known: true, InputTokens: 1000}
	c.session.model = &compactModel{err: &ModelError{Status: StatusProviderError, Message: "provider down", Usage: usage}}
	c.commandModel(context.Background(), "2", nil, io.Discard)
	if c.cfg.Model != modelCatalog[1].ID || len(c.session.history) != 0 || c.usage != usage {
		t.Fatalf("session %+v, usage %+v", c.session, c.usage)
	}
	var stderr bytes.Buffer
	c.commandStatus(&stderr)
	if !strings.Contains(stderr.String(), "123 in (80 cached) · 7 out") {
		t.Fatalf("status: %s", stderr.String())
	}
}

func autoCompactConversation(t *testing.T, model Model, lastInput int64) *conversation {
	t.Helper()
	c := newConversation(t, "gpt-6-luna", "low", 1)
	c.session.model = model
	c.session.lastRequest = Usage{Known: true, InputTokens: lastInput}
	c.usage = Usage{Known: true}
	return c
}

func TestChat_AutoCompactAtThreshold(t *testing.T) {
	model := &compactModel{replies: []ModelResponse{turn(textBlock("handoff")), turn(textBlock("done"))}}
	c := autoCompactConversation(t, model, 840_000)
	var stdout, stderr bytes.Buffer
	c.session.display = NewDisplay(&stderr)
	c.runTurn(context.Background(), "next", nil, &stdout, &stderr)
	if len(model.requests) != 2 || model.requests[0].History[len(model.requests[0].History)-1].User.Text != compactPrompt {
		t.Fatalf("requests: %+v", model.requests)
	}
	after := model.requests[1].History
	if len(after) != 2 || after[0].Kind != EntrySummary || after[1].User.Text != "next" {
		t.Fatalf("history after compaction: %+v", after)
	}
	if !strings.Contains(stderr.String(), "context 80% of the window; compacting before this turn") || !strings.Contains(stderr.String(), "compacted:") {
		t.Fatalf("stderr: %s", stderr.String())
	}
}

func TestChat_AutoCompactSkipsWhenItShould(t *testing.T) {
	for _, test := range []struct {
		name   string
		last   int64
		mutate func(*conversation)
	}{
		{"below threshold", 839_999, func(*conversation) {}},
		{"unknown window", 900_000, func(c *conversation) { c.cfg.Model = "custom-model" }},
		{"blocked", 900_000, func(c *conversation) { c.session.blocked = string(StatusProtocolError) }},
		{"empty history", 900_000, func(c *conversation) { c.session.history = nil }},
		{"usage unknown", 0, func(c *conversation) { c.session.lastRequest = Usage{} }},
	} {
		t.Run(test.name, func(t *testing.T) {
			c := autoCompactConversation(t, &compactModel{}, test.last)
			test.mutate(c)
			if c.shouldAutoCompact() {
				t.Fatal("auto-compaction would run")
			}
		})
	}
}

func TestChat_AutoCompactBelowThresholdSendsOneRequest(t *testing.T) {
	model := &compactModel{replies: []ModelResponse{turn(textBlock("done"))}}
	c := autoCompactConversation(t, model, 839_999)
	var stdout, stderr bytes.Buffer
	c.session.display = NewDisplay(&stderr)
	c.runTurn(context.Background(), "next", nil, &stdout, &stderr)
	if len(model.requests) != 1 || strings.Contains(stderr.String(), "compacting") {
		t.Fatalf("requests %d, stderr %s", len(model.requests), stderr.String())
	}
}

func TestChat_AutoCompactFailureStillSendsTurn(t *testing.T) {
	model := &compactModel{err: fmt.Errorf("provider down")}
	c := autoCompactConversation(t, model, 900_000)
	old := len(c.session.history)
	var stderr bytes.Buffer
	if !c.autoCompact(context.Background(), &stderr) {
		t.Fatal("a failed compaction stopped the turn")
	}
	if len(c.session.history) != old || c.session.history[0].Kind == EntrySummary || !strings.Contains(stderr.String(), "auto-compaction failed: provider down; sending the message anyway") {
		t.Fatalf("history %+v, stderr %s", c.session.history, stderr.String())
	}
}

func TestChat_AutoCompactCancelledDropsTurn(t *testing.T) {
	model := &compactModel{err: &ModelError{Status: StatusCancelled, Message: "cancelled"}}
	c := autoCompactConversation(t, model, 900_000)
	old := len(c.session.history)
	var stdout, stderr bytes.Buffer
	c.runTurn(context.Background(), "next", nil, &stdout, &stderr)
	if len(model.requests) != 1 || len(c.session.history) != old || !strings.Contains(stderr.String(), "message not sent") {
		t.Fatalf("requests %d, history %+v, stderr %s", len(model.requests), c.session.history, stderr.String())
	}
}

func TestChat_AutoCompactDoesNotRepeat(t *testing.T) {
	summary := turn(textBlock("handoff"))
	summary.Usage = Usage{Known: true, InputTokens: 900_000}
	model := &compactModel{replies: []ModelResponse{summary, turn(textBlock("one")), turn(textBlock("two"))}}
	c := autoCompactConversation(t, model, 900_000)
	var stdout, stderr bytes.Buffer
	c.session.display = NewDisplay(&stderr)
	c.runTurn(context.Background(), "a", nil, &stdout, &stderr)
	c.runTurn(context.Background(), "b", nil, &stdout, &stderr)
	if len(model.requests) != 3 || strings.Count(stderr.String(), "compacting before") != 1 {
		t.Fatalf("requests %d, stderr %s", len(model.requests), stderr.String())
	}
}
