package reagent

import (
	"bytes"
	"os"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestDisplay_PlanBlockDropsTheTags(t *testing.T) {
	var out, errs bytes.Buffer
	d := NewDisplay(&errs)
	reply := "A note.\n<plan>\n- Change parser\n- Test parser\n</plan>\nDone."
	d.reply(&out, reply, true)
	if strings.Contains(out.String(), "<plan>") || strings.Contains(out.String(), "</plan>") || !strings.Contains(out.String(), "plan\n- Change parser\n- Test parser") || !strings.Contains(out.String(), "A note.") || !strings.Contains(out.String(), "Done.") {
		t.Fatalf("rendered: %q", out.String())
	}
}

func TestDisplay_PlainOutputHasNoEscapes(t *testing.T) {
	var b bytes.Buffer
	d := NewDisplay(&b)
	d.summary(RunResult{Status: StatusCompleted, Steps: 1}, time.Second, false, false)
	if bytes.Contains(b.Bytes(), []byte("\x1b")) {
		t.Fatal(b.String())
	}
}
func TestDisplay_SummaryLine(t *testing.T) {
	cases := []struct {
		result RunResult
		want   string
	}{
		{RunResult{Status: StatusCompleted, Steps: 1, ToolCalls: 1, Usage: Usage{Known: true, InputTokens: 1200, CachedInputTokens: 0, OutputTokens: 3}}, "✓ completed · 1 step · 1 tool call · 1.2k in (0 cached) · 3 out · 1.0s\n"},
		{RunResult{Status: StatusLimitExceeded, Steps: 2, Reason: "no_followup_step"}, "✗ limit_exceeded · 2 steps · 0 tool calls · tokens unknown · 1.0s\n  the step budget ran out\n"},
		{RunResult{Status: StatusEffectUnknown}, "! effect_unknown · 0 steps · 0 tool calls · tokens unknown · 1.0s\n"},
	}
	for _, test := range cases {
		var b bytes.Buffer
		NewDisplay(&b).summary(test.result, time.Second, false, false)
		if got := b.String(); got != test.want {
			t.Fatalf("got %q, want %q", got, test.want)
		}
	}
}

func TestDisplay_BrowsingSummaryReplacesSuccessfulActivity(t *testing.T) {
	var b bytes.Buffer
	d := NewDisplay(&b)
	d.beginTurn()
	for _, tc := range []struct {
		name, path, workspace string
	}{
		{"read_file", "a.go", "/one"},
		{"read_file", "a.go", "/one"}, // another range of the same file
		{"read_file", "a.go", "/two"}, // the same path in another workspace
		{"search_text", ".", "/one"},
		{"list_files", ".", "/one"},
	} {
		d.toolFinished(ToolCall{Name: tc.name, Arguments: `{"path":"` + tc.path + `"}`}, ToolOutcome{OK: true, Workspace: tc.workspace, Data: []byte(`{}`)})
	}
	if got := b.String(); got != "" {
		t.Fatalf("successful browsing printed activity: %q", got)
	}
	d.toolFinished(ToolCall{Name: "read_file", Arguments: `{"path":"missing.go"}`}, failOutcome("not_found", "missing"))
	d.toolFinished(ToolCall{Name: "search_text", Arguments: `{"path":".","query":"q"}`}, failOutcome("invalid_arguments", "bad query"))
	d.summary(RunResult{Status: StatusCompleted}, time.Second, false, false)
	if got := b.String(); got != "  ✗ read_file missing.go → not_found: missing\n  ✗ search_text \"q\" in . → invalid_arguments: bad query\n✓ completed · 0 steps · 0 tool calls · tokens unknown · 1.0s\n  ✓ read 2 files · 1 search · 1 listing\n" {
		t.Fatalf("browsing summary: %q", got)
	}

	b.Reset()
	d.beginTurn()
	d.toolFinished(ToolCall{Name: "list_files", Arguments: `{"path":"."}`}, ToolOutcome{OK: true, Data: []byte(`{}`)})
	d.summary(RunResult{Status: StatusProviderError}, time.Second, false, false)
	if got := b.String(); !strings.Contains(got, "  ✓ 1 listing\n") || strings.Contains(got, "read 2 files") {
		t.Fatalf("next turn summary: %q", got)
	}
}

func TestDisplay_ContextWarningAtSixtyPercent(t *testing.T) {
	for _, test := range []struct {
		name   string
		usage  Usage
		window int64
		auto   bool
		want   string
	}{
		{"below", Usage{Known: true, InputTokens: 599_999}, 1_000_000, false, ""},
		{"at", Usage{Known: true, InputTokens: 600_000}, 1_000_000, false, "  context 60% of the window; /compact summarizes or /reset starts over\n"},
		{"above", Usage{Known: true, InputTokens: 630_000}, 1_000_000, false, "  context 63% of the window; /compact summarizes or /reset starts over\n"},
		{"auto compacts next", Usage{Known: true, InputTokens: 860_000}, 1_000_000, true, "  context 86% of the window; compacts before your next message\n"},
		{"disarmed above eighty", Usage{Known: true, InputTokens: 860_000}, 1_000_000, false, "  context 86% of the window; /compact summarizes or /reset starts over\n"},
		{"unknown window", Usage{Known: true, InputTokens: 900_000}, 0, false, ""},
		{"unknown usage", Usage{}, 1_000_000, false, ""},
	} {
		t.Run(test.name, func(t *testing.T) {
			var b bytes.Buffer
			NewDisplay(&b).contextWarning(test.usage, test.window, test.auto)
			if got := b.String(); got != test.want {
				t.Fatalf("got %q, want %q", got, test.want)
			}
		})
	}
}

func TestDisplay_BlockedGuidanceDependsOnStatus(t *testing.T) {
	for _, test := range []struct {
		status RunStatus
		want   string
	}{
		{StatusLimitExceeded, "session blocked: /reset to continue; /compact works before the window fills (watch the 60% warning)"},
		{StatusProtocolError, "session blocked: /compact or /reset to continue"},
	} {
		var b bytes.Buffer
		NewDisplay(&b).blocked(test.status, "trace.jsonl")
		if !strings.Contains(b.String(), test.want) || !strings.Contains(b.String(), "trace.jsonl") {
			t.Fatalf("%s: %q", test.status, b.String())
		}
	}
}

func TestDisplay_HidesRecapWhenNotRequested(t *testing.T) {
	var b bytes.Buffer
	d := NewDisplay(&b)
	d.recap = []string{"changed config.txt", "ran go test ./..."}
	d.summary(RunResult{Status: StatusCompleted}, time.Second, false, false)
	if strings.Contains(b.String(), "changed config.txt") || strings.Contains(b.String(), "go test ./...") {
		t.Fatalf("recap printed without being requested: %q", b.String())
	}
}

func TestDisplay_AlignsRecapVerbs(t *testing.T) {
	var b bytes.Buffer
	d := NewDisplay(&b)
	d.recap = []string{"changed config.txt", "ran go test ./..."}
	d.summary(RunResult{Status: StatusCompleted}, time.Second, false, true)
	if got := b.String(); !bytes.Contains([]byte(got), []byte("  changed config.txt\n  ran     go test ./...\n")) {
		t.Fatalf("recap is not aligned: %q", got)
	}
}

func TestFormatCount(t *testing.T) {
	for _, test := range []struct {
		in   int64
		want string
	}{{1200, "1.2k"}, {999_950, "1.0M"}} {
		if got := formatCount(test.in); got != test.want {
			t.Fatalf("formatCount(%d) = %q, want %q", test.in, got, test.want)
		}
	}
}
func TestShortPath(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skip(err)
	}
	if got, want := shortPath(home+"/project"), "~/project"; got != want {
		t.Fatalf("got %q, want %q", got, want)
	}
	if got := shortPath(home + "2/project"); got != home+"2/project" {
		t.Fatalf("path-prefix match incorrectly shortened %q", got)
	}
}

func TestFormatElapsed(t *testing.T) {
	if got := formatElapsed(65 * time.Second); got != "1m05s" {
		t.Fatal(got)
	}
}

func TestStatusText(t *testing.T) {
	stripStyle := func(s string) string {
		s = strings.ReplaceAll(s, ansiDim, "")
		return strings.ReplaceAll(s, ansiReset, "")
	}
	cases := []struct {
		name    string
		frame   int
		elapsed time.Duration
		columns int
		want    string
	}{
		{"first frame without elapsed", 0, 999 * time.Millisecond, 0, "⠋ waiting"},
		{"cycles frames and adds elapsed", 11, time.Second, 0, "⠙ waiting · 1.0s"},
		{"caps terminal width", 0, time.Second, 12, "⠋ waiting …"},
		{"one column leaves room for cursor", 0, time.Second, 1, ""},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			got := stripStyle(statusText(test.frame, "waiting", test.elapsed, test.columns))
			if got != test.want {
				t.Errorf("statusText() = %q, want %q", got, test.want)
			}
			if test.columns > 1 && displayWidth(got) > test.columns-1 {
				t.Errorf("status width = %d, want at most %d", displayWidth(got), test.columns-1)
			}
		})
	}
}

func TestDisplay_StatusLineStopsCleanly(t *testing.T) {
	// The ticker is stopped and joined before this buffer is read, so it does
	// not need its own synchronization.
	var b bytes.Buffer
	d := NewDisplay(&b)
	d.live = true // A buffer is not a terminal; enable the live path explicitly.
	d.tick = time.Millisecond
	d.modelStarted("test-model", 1, 2)
	time.Sleep(20 * time.Millisecond)
	d.stopStatus()

	if got := b.String(); !strings.HasSuffix(got, "\r\x1b[2K") {
		t.Fatalf("status output does not end by erasing the line: %q", got)
	}
	length := b.Len()
	time.Sleep(20 * time.Millisecond)
	if got := b.Len(); got != length {
		t.Fatalf("status wrote after stop: length %d, want %d", got, length)
	}
}

func TestDisplay_PlainNeverDrawsStatus(t *testing.T) {
	var b bytes.Buffer
	d := NewDisplay(&b)
	d.modelStarted("test-model", 1, 2)
	d.modelFinished()
	if got := b.String(); got != "" {
		t.Fatalf("plain display drew status: %q", got)
	}
}

func TestDisplay_ProgressUsesRegionStatusWhileTurnRuns(t *testing.T) {
	var out bytes.Buffer
	r := terminalInput(strings.NewReader(""))
	r.region = &terminalRegion{out: &out, mu: &sync.Mutex{}}
	d := NewDisplay(&out)
	d.attachRegion(r)
	r.regionStatus = regionStatus{model: "scripted"}
	rows, row, col := regionInputRows(nil, 0, 40, r.regionStatus, false)
	r.region.draw(rows, row, col, 40, 12)
	d.tick = time.Hour
	d.startStatus("waiting for scripted · step 1")
	if !strings.Contains(r.regionStatus.progress, "waiting for scripted") {
		t.Fatal("progress was not published to the region")
	}
	d.stopStatus()
	if r.regionStatus.progress != "" {
		t.Fatalf("progress remained after stop: %q", r.regionStatus.progress)
	}
}
