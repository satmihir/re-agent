package reagent

import "testing"

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
	got := describeActivity(call, ToolOutcome{Code: "not_found", Message: "missing"}).render(false, 0)
	if got != "  ✗ read_file a\\x1b[2Jb → not_found: missing\n" {
		t.Fatal(got)
	}
}
