package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestShell_RecordsOutputAndExitStatus(t *testing.T) {
	var live bytes.Buffer
	got, err := runShellCommand(context.Background(), t.TempDir(), "echo out; echo err >&2; exit 3", &live)
	if err != nil {
		t.Fatal(err)
	}
	// Both streams share one writer, so the record keeps the order they ran in.
	if got.Output != "out\nerr\n" || live.String() != got.Output {
		t.Fatalf("output %q, live %q", got.Output, live.String())
	}
	if got.ExitCode == nil || *got.ExitCode != 3 || got.Signal != nil || got.Interrupted {
		t.Fatalf("got %+v", got)
	}
	if got.Kind != "user_shell_command" || got.Command != "echo out; echo err >&2; exit 3" {
		t.Fatalf("got %+v", got)
	}
}

func TestShell_RunsInTheWorkspace(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "marker.txt"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	got, err := runShellCommand(context.Background(), dir, "ls", &bytes.Buffer{})
	if err != nil || got.Output != "marker.txt\n" {
		t.Fatalf("got %q, %v", got.Output, err)
	}
}

// The model sees what a ! command prints, so the key must not be printable.
func TestShell_EnvironmentIsAllowlisted(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "sk-must-not-leak")
	got, err := runShellCommand(context.Background(), t.TempDir(), "env", &bytes.Buffer{})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(got.Output, "sk-must-not-leak") || !strings.Contains(got.Output, "PATH=") {
		t.Fatalf("env printed %q", got.Output)
	}
}

func TestShell_OutputTrimmedToResultBudget(t *testing.T) {
	var live bytes.Buffer
	got, err := runShellCommand(context.Background(), t.TempDir(), "head -c 100000 /dev/zero | tr '\\0' x", &live)
	if err != nil {
		t.Fatal(err)
	}
	encoded, _ := json.Marshal(got)
	if len(encoded) > MaxResultBytes || !got.OutputTruncated || got.OutputBytesSeen != 100000 {
		t.Fatalf("encoded %d bytes, truncated %v, seen %d", len(encoded), got.OutputTruncated, got.OutputBytesSeen)
	}
	// Only the record is bounded; the terminal saw everything.
	if live.Len() != 100000 {
		t.Fatalf("live output was %d bytes", live.Len())
	}
}

func TestShell_CancelRecordsInterrupted(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer cancel()
	started := time.Now()
	got, err := runShellCommand(ctx, t.TempDir(), "echo begun; sleep 30", &bytes.Buffer{})
	if err != nil {
		t.Fatal(err)
	}
	if !got.Interrupted || got.Output != "begun\n" || time.Since(started) > 5*time.Second {
		t.Fatalf("got %+v after %v", got, time.Since(started))
	}
}

// A background job that keeps the output open does not make the command
// itself look unfinished.
func TestShell_BackgroundJobDoesNotHoldTheCommand(t *testing.T) {
	got, err := runShellCommand(context.Background(), t.TempDir(), "(sleep 3 &); echo started", &bytes.Buffer{})
	if err != nil {
		t.Fatal(err)
	}
	if got.ExitCode == nil || *got.ExitCode != 0 || got.Interrupted || got.Output != "started\n" {
		t.Fatalf("got %+v", got)
	}
}

func TestShell_UnstartedCommandIsAnError(t *testing.T) {
	missing := filepath.Join(t.TempDir(), "gone")
	if _, err := runShellCommand(context.Background(), missing, "true", &bytes.Buffer{}); err == nil {
		t.Fatal("a command in a missing directory was reported as run")
	}
}

func shellEntry(output string) Entry {
	zero := 0
	return Entry{Kind: EntryShell, Shell: &ShellCommand{
		Kind: "user_shell_command", Command: "go vet ./...", ExitCode: &zero, Output: output,
	}}
}

func TestEncodeOpenAI_ShellCommandIsAUserMessage(t *testing.T) {
	entry := shellEntry("vet: ok\n")
	_, got := encode(t, ModelRequest{History: []Entry{entry, {Kind: EntryUser, User: &UserTurn{Text: "and now?"}}}})

	want, err := shellCommandText(*entry.Shell)
	if err != nil {
		t.Fatal(err)
	}
	var message responsesMessage
	if err := json.Unmarshal(got.Input[0], &message); err != nil {
		t.Fatal(err)
	}
	if len(got.Input) != 2 || message.Role != "user" || len(message.Content) != 1 || message.Content[0].Text != want {
		t.Fatalf("got %d items, first %s", len(got.Input), got.Input[0])
	}
	if !strings.HasPrefix(want, shellPreamble+`{"kind":"user_shell_command","command":"go vet ./..."`) {
		t.Fatalf("text %q", want)
	}
}

// The Messages API expects user and assistant to alternate, so a command and
// the message typed after it travel as one user message.
func TestEncodeAnthropic_ShellCommandJoinsTheNextUserMessage(t *testing.T) {
	entry := shellEntry("vet: ok\n")
	final := ModelResponse{Native: NativeOutput{Provider: anthropicProvider,
		Items: []json.RawMessage{json.RawMessage(`{"type":"text","text":"done"}`)}}}
	_, got := encodeAnthropic(t, ModelRequest{History: []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "the task"}},
		{Kind: EntryAssistant, Assistant: &final},
		entry,
		{Kind: EntryUser, User: &UserTurn{Text: "and now?"}},
	}})

	if len(got.Messages) != 3 || got.Messages[0].Role != "user" || got.Messages[1].Role != "assistant" || got.Messages[2].Role != "user" {
		t.Fatalf("got %+v", got.Messages)
	}
	want, err := shellCommandText(*entry.Shell)
	if err != nil {
		t.Fatal(err)
	}
	var blocks []messagesTextBlock
	for _, raw := range got.Messages[2].Content {
		var block messagesTextBlock
		if err := json.Unmarshal(raw, &block); err != nil {
			t.Fatal(err)
		}
		blocks = append(blocks, block)
	}
	if len(blocks) != 2 || blocks[0].Text != want || blocks[1].Text != "and now?" {
		t.Fatalf("got %+v", blocks)
	}
	// The cache point stays on the last block of the request.
	if blocks[0].CacheControl != nil || blocks[1].CacheControl == nil {
		t.Fatalf("cache control on %+v", blocks)
	}
}
