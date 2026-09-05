package reagent

import (
	"bytes"
	"context"
	"path/filepath"
	"strings"
	"testing"
)

func TestMain_ScriptedRunPrintsReplyOnStdout(t *testing.T) {
	var stdout, stderr bytes.Buffer
	trace := filepath.Join(t.TempDir(), "events.jsonl")

	code := Main(context.Background(), []string{"run",
		"--scripted", "../../testdata/scripts/echo_then_answer.json",
		"--trace-file", trace,
		"Where is the timeout set?"}, &stdout, &stderr)

	if code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}
	if got := strings.TrimSpace(stdout.String()); got != "The tool returned: the timeout is 30s." {
		t.Fatalf("stdout: %q", got)
	}
	if !strings.Contains(stderr.String(), "trace: "+trace) {
		t.Fatalf("stderr: %q", stderr.String())
	}
}

func TestMain_UsageErrors(t *testing.T) {
	cases := map[string][]string{
		"no subcommand": {},
		"no prompt":     {"run", "--scripted", "x.json"},
		"no script":     {"run", "a prompt"},
		"missing file":  {"run", "--scripted", "absent.json", "a prompt"},
		"zero steps":    {"run", "--scripted", "x.json", "--max-steps", "0", "a prompt"},
	}
	for name, args := range cases {
		t.Run(name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			if code := Main(context.Background(), args, &stdout, &stderr); code != exitUsage {
				t.Fatalf("exit %d, want %d", code, exitUsage)
			}
		})
	}
}
