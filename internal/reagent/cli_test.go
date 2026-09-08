package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
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
		"Where is the timeout set?"}, strings.NewReader(""), &stdout, &stderr)

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
		"missing file":  {"run", "--scripted", "absent.json", "a prompt"},
		"zero steps":    {"run", "--scripted", "x.json", "--max-steps", "0", "a prompt"},
	}
	for name, args := range cases {
		t.Run(name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			if code := Main(context.Background(), args, strings.NewReader(""), &stdout, &stderr); code != exitUsage {
				t.Fatalf("exit %d, want %d", code, exitUsage)
			}
		})
	}
}

// The preview must be the request itself, not a description of it, so it is
// compared byte for byte against the encoder the live path will use (v0 §6.1).
func TestMain_ShowContextMatchesTheEncoderByte(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "")
	t.Setenv("REAGENT_MODEL", "")
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "a.txt"), []byte("hi\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run",
		"--workspace", root, "--show-context", "Where is the timeout set?"}, strings.NewReader(""), &stdout, &stderr)
	if code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}

	ws, err := OpenWorkspace(root)
	if err != nil {
		t.Fatal(err)
	}
	registry, err := NewRegistry(Mode{}, NewListFilesTool(ws), NewReadFileTool(ws), NewSearchTextTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	want, err := PreviewRequest(Config{
		Provider: openaiName, Model: DefaultOpenAIModel, ReasoningEffort: DefaultReasoningEffort,
		Registry: registry, WorkspacePath: ws.Root(),
		MaxSteps: 20, MaxToolCalls: 40,
	}, "Where is the timeout set?")
	if err != nil {
		t.Fatal(err)
	}
	if got := bytes.TrimSuffix(stdout.Bytes(), []byte("\n")); !bytes.Equal(got, want) {
		t.Fatalf("preview differs from the encoder output\ngot  %s\nwant %s", got, want)
	}
}

// A preview contacts nothing and needs no credentials.
func TestMain_ShowContextNeedsNoCredentials(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "secret-key-value")
	var stdout, stderr bytes.Buffer

	if code := Main(context.Background(), []string{"run",
		"--workspace", t.TempDir(), "--show-context", "a task"}, strings.NewReader(""), &stdout, &stderr); code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}
	if strings.Contains(stdout.String(), "secret-key-value") {
		t.Fatal("the preview leaked the API key")
	}
	if strings.Contains(stdout.String(), "Authorization") {
		t.Fatal("the preview carries a transport header")
	}
}

func TestMain_ShowContextReportsAnOversizedRequest(t *testing.T) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run",
		"--workspace", t.TempDir(), "--show-context",
		strings.Repeat("x ", MaxRequestBytes/2)}, strings.NewReader(""), &stdout, &stderr)

	if code != exitRunFail {
		t.Fatalf("exit %d, want %d", code, exitRunFail)
	}
	if !strings.Contains(stderr.String(), "over the") || stdout.Len() != 0 {
		t.Fatalf("stdout %d bytes, stderr: %s", stdout.Len(), stderr.String())
	}
}

func TestMain_ConflictingAndMissingModes(t *testing.T) {
	cases := map[string][]string{
		"script with model":   {"run", "--scripted", "x.json", "--model", "m", "a task"},
		"script with preview": {"run", "--scripted", "x.json", "--show-context", "a task"},
		"no model source":     {"run", "a task"},
	}
	for name, args := range cases {
		t.Run(name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			if code := Main(context.Background(), args, strings.NewReader(""), &stdout, &stderr); code != exitUsage {
				t.Fatalf("exit %d, want %d", code, exitUsage)
			}
		})
	}
}

// Write mode is granted at launch, and the preview shows exactly which tools
// that grant declared.
func TestMain_AllowWriteDeclaresTheEditTool(t *testing.T) {
	t.Setenv("REAGENT_MODEL", "")
	root := t.TempDir()

	declared := func(args ...string) string {
		t.Helper()
		var stdout, stderr bytes.Buffer
		full := append([]string{"run", "--workspace", root, "--show-context"}, args...)
		if code := Main(context.Background(), append(full, "a task"), strings.NewReader(""), &stdout, &stderr); code != exitOK {
			t.Fatalf("exit %d, stderr: %s", code, stderr.String())
		}
		var request decodedRequest
		if err := json.Unmarshal(bytes.TrimSpace(stdout.Bytes()), &request); err != nil {
			t.Fatal(err)
		}
		var names []string
		for _, tool := range request.Tools {
			names = append(names, tool.Name)
		}
		return strings.Join(names, ",") + " | " + request.Instructions[strings.Index(request.Instructions, "Mode: "):]
	}

	if got := declared(); got != "list_files,read_file,search_text | Mode: read only\n" {
		t.Fatalf("read-only run declared %q", got)
	}
	if got := declared("--allow-write"); got != "edit_file,list_files,read_file,search_text | Mode: read and write\n" {
		t.Fatalf("write run declared %q", got)
	}
}
