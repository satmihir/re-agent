package reagent

import (
	"bytes"
	"os"
	"testing"
	"time"
)

func TestDisplay_PlainOutputHasNoEscapes(t *testing.T) {
	var b bytes.Buffer
	d := NewDisplay(&b)
	d.summary(RunResult{Status: StatusCompleted, Steps: 1}, time.Second, false)
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
		NewDisplay(&b).summary(test.result, time.Second, false)
		if got := b.String(); got != test.want {
			t.Fatalf("got %q, want %q", got, test.want)
		}
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
