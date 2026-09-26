package reagent

import (
	"strings"
	"testing"
)

func TestActivity_FailedCallsUseToolTargets(t *testing.T) {
	tests := []struct {
		call    ToolCall
		code    string
		message string
		want    string
	}{
		{ToolCall{Name: "read_file", Arguments: `{"path":"missing.txt"}`}, "not_found", "missing", "  ✗ read_file missing.txt → not_found: missing\n"},
		{ToolCall{Name: "edit_file", Arguments: `{"path":"conf.txt","old_text":"x","new_text":"y"}`}, "stale_file", "stale", "  ✗ edit_file conf.txt → stale_file: stale\n"},
		{ToolCall{Name: "exec", Arguments: `{"argv":["sh","-c","exit 3"],"cwd":"."}`}, "command_failed", "failed", "  ✗ exec sh -c 'exit 3' → command_failed: failed\n"},
	}
	for _, test := range tests {
		if got := describeActivity(test.call, ToolOutcome{Code: test.code, Message: test.message}).render(false, 0); got != test.want {
			t.Errorf("got %q, want %q", got, test.want)
		}
	}
}

func TestActivity_ModelTextCannotStyleTerminal(t *testing.T) {
	call := ToolCall{Name: "read_file", Arguments: "{\"path\":\"a\\u001b[2Jb\"}"}
	plain := describeActivity(call, ToolOutcome{Code: "not_found", Message: "missing"}).render(false, 0)
	if plain != "  ✗ read_file a\\x1b[2Jb → not_found: missing\n" {
		t.Fatal(plain)
	}
	styled := describeActivity(call, ToolOutcome{Code: "not_found", Message: "missing"}).render(true, 0)
	for _, sequence := range []string{ansiGreen, ansiRed, ansiYellow, ansiDim, ansiReset} {
		styled = strings.ReplaceAll(styled, sequence, "")
	}
	if strings.Contains(styled, "\x1b") {
		t.Fatalf("model text emitted an escape sequence: %q", styled)
	}
}

func TestActivity_DescribesEachTool(t *testing.T) {
	tests := []struct {
		name string
		call ToolCall
		data string
		want string
	}{
		{"read", ToolCall{Name: "read_file", Arguments: `{"path":"a"}`}, `{"total_lines":2,"lines":[{"number":1},{"number":2}],"eof":true}`, "  ✓ read_file a → 2 lines\n"},
		{"read range", ToolCall{Name: "read_file", Arguments: `{"path":"a"}`}, `{"total_lines":9,"lines":[{"number":3},{"number":4}]}`, "  ✓ read_file a → lines 3-4 of 9\n"},
		{"list", ToolCall{Name: "list_files", Arguments: `{"path":"."}`}, `{"entries":[{}]}`, "  ✓ list_files . → 1 entry\n"},
		{"list more", ToolCall{Name: "list_files", Arguments: `{"path":"."}`}, `{"entries":[{},{}],"next_offset":2}`, "  ✓ list_files . → 2 entries, more\n"},
		{"search", ToolCall{Name: "search_text", Arguments: `{"query":"q","path":"."}`}, `{"matches":[{"path":"a"},{"path":"b"}],"complete":true}`, "  ✓ search_text \"q\" in . → 2 matches in 2 files\n"},
		{"search incomplete", ToolCall{Name: "search_text", Arguments: `{"query":"q","path":"."}`}, `{"matches":[],"complete":false}`, "  ✓ search_text \"q\" in . → no matches, incomplete\n"},
		{"edit", ToolCall{Name: "edit_file", Arguments: `{"path":"a","old_text":"x","new_text":"y"}`}, `{"changed":true}`, "  ✓ edit_file a → +1 -1\n      - x\n      + y\n"},
		{"exec", ToolCall{Name: "exec", Arguments: `{"argv":["true"],"cwd":"."}`}, `{"exit_code":0,"duration_ms":500}`, "  ✓ exec true → exit 0 in 0.5s\n"},
		{"echo", ToolCall{Name: "echo", Arguments: `{"text":"hello"}`}, `{}`, "  ✓ echo \"hello\"\n"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got := describeActivity(test.call, ToolOutcome{OK: true, Data: []byte(test.data)}).render(false, 0)
			if got != test.want {
				t.Fatalf("got %q, want %q", got, test.want)
			}
		})
	}
}

func TestActivity_RegexSearchTarget(t *testing.T) {
	regex := ToolCall{Name: "search_text", Arguments: `{"query":"a|b","regex":true,"path":"."}`}
	if got, want := callTarget(regex), "/a|b/ in ."; got != want {
		t.Fatalf("regex target: got %q, want %q", got, want)
	}
	if got, want := describeActivity(regex, ToolOutcome{OK: true, Data: []byte(`{"matches":[],"complete":true}`)}).render(false, 0), "  ✓ search_text /a|b/ in . → no matches\n"; got != want {
		t.Fatalf("regex activity: got %q, want %q", got, want)
	}

	literal := ToolCall{Name: "search_text", Arguments: `{"query":"a|b","path":"."}`}
	if got, want := callTarget(literal), `"a|b" in .`; got != want {
		t.Fatalf("literal target: got %q, want %q", got, want)
	}
}

func TestActivity_ExecFailureAndTimeout(t *testing.T) {
	call := ToolCall{Name: "exec", Arguments: `{"argv":["sh","-c","exit 3"],"cwd":"."}`}
	tests := []struct {
		name    string
		outcome ToolOutcome
		want    string
	}{
		{"failed exit", ToolOutcome{Code: "command_failed", Data: []byte(`{"exit_code":3,"duration_ms":20}`)}, "  ✗ exec sh -c 'exit 3' → exit 3 in 0.0s\n"},
		{"timeout", ToolOutcome{Code: "timeout", Data: []byte(`{"duration_ms":2000}`)}, "  ! exec sh -c 'exit 3' → timed out after 2.0s; effects unknown\n"},
		{"not executed", ToolOutcome{Code: "not_executed", Message: "step budget reached"}, "  – exec sh -c 'exit 3' → not run: step budget reached\n"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if got := describeActivity(call, test.outcome).render(false, 0); got != test.want {
				t.Fatalf("got %q, want %q", got, test.want)
			}
		})
	}
}

func TestActivity_CommandQuoting(t *testing.T) {
	got := commandText(execArgs{Argv: []string{"sh", "a b", "it's", "", "plain"}, Cwd: "sub dir"})
	want := "sh 'a b' 'it'\\''s' '' plain in sub dir"
	if got != want {
		t.Fatalf("got %q, want %q", got, want)
	}
}

func TestActivity_MultiLineCommandIsOneRow(t *testing.T) {
	args := execArgs{Argv: []string{"gh", "pr", "create", "--body", "## Summary\n- one"}, Cwd: "a\nb"}
	if got, want := commandText(args), "gh pr create --body '## Summary↵- one' in a↵b"; got != want {
		t.Fatalf("command: got %q, want %q", got, want)
	}

	call := ToolCall{Name: "exec", Arguments: `{"argv":["gh","pr","create","--body","## Summary\n- one"],"cwd":"a\nb"}`}
	outcome := ToolOutcome{OK: true, Effect: EffectApplied, Data: []byte(`{}`)}
	if got := describeActivity(call, outcome).render(false, 0); strings.Count(got, "\n") != 1 {
		t.Fatalf("activity has %d newlines: %q", strings.Count(got, "\n"), got)
	}
	if got := recapLine(call, outcome); strings.Contains(got, "\n") {
		t.Fatalf("recap has a newline: %q", got)
	}
	if got := statusText(0, "running "+callTarget(call), 0, 80); strings.Contains(got, "\n") {
		t.Fatalf("status has a newline: %q", got)
	}

	search := ToolCall{Name: "search_text", Arguments: `{"query":"a\nb","path":"."}`}
	if got, want := callTarget(search), `"a↵b" in .`; got != want {
		t.Fatalf("search target: got %q, want %q", got, want)
	}
	if got := describeActivity(search, ToolOutcome{OK: true, Data: []byte(`{"matches":[],"complete":true}`)}).render(false, 0); strings.Count(got, "\n") != 1 {
		t.Fatalf("search activity has %d newlines: %q", strings.Count(got, "\n"), got)
	}
}

func TestActivity_EditPreviewDropsSharedLinesAndCaps(t *testing.T) {
	old := "same\nold\ntail"
	new := "same\nnew\ntail"
	removed, added := changedLines(old, new)
	if got := previewLines(removed, added); strings.Join(got, "\n") != "- old\n+ new" {
		t.Fatalf("preview: %q", got)
	}
	many := make([]string, 7)
	if got := previewLines(many, nil); len(got) != 7 || got[6] != "… 1 more line" {
		t.Fatalf("capped preview: %q", got)
	}
}

func TestActivity_TruncatesToColumns(t *testing.T) {
	a := activity{mark: markOK, tool: "search_text", target: strings.Repeat("界", 40), result: strings.Repeat("result", 20), preview: []string{"+ " + strings.Repeat("x", 50)}}
	for _, line := range strings.Split(strings.TrimSuffix(a.render(true, 40), "\n"), "\n") {
		for _, sequence := range []string{ansiGreen, ansiRed, ansiYellow, ansiDim, ansiReset} {
			line = strings.ReplaceAll(line, sequence, "")
		}
		if displayWidth(line) > 40 {
			t.Fatalf("line is %d cells: %q", displayWidth(line), line)
		}
	}
}
