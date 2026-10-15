package reagent

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
)

func TestMain_OwnedFlagErrors(t *testing.T) {
	for _, tc := range []struct {
		name string
		args []string
		want string
	}{
		{"integer", []string{"--max-steps=nope"}, "--max-steps expects an integer"},
		{"integer range", []string{"--max-tool-calls", "999999999999999999999999"}, "--max-tool-calls expects an integer"},
		{"boolean", []string{"--read-only=maybe"}, "--read-only expects a boolean (true or false)"},
		{"duration", []string{"--model-retry-window", "later"}, "--model-retry-window expects a duration (for example 30s or 12h)"},
		{"missing integer", []string{"--max-steps"}, "--max-steps requires an integer"},
		{"missing string", []string{"--model"}, "--model requires text"},
		{"missing repeated", []string{"--allow-workspace"}, "--allow-workspace requires text"},
		{"unknown", []string{"--not-a-flag"}, "unknown flag --not-a-flag"},
		{"single dash", []string{"-max-steps=nope"}, "--max-steps expects an integer"},
		{"malformed", []string{"---max-steps"}, `invalid flag name "---max-steps"`},
		{"after valid", []string{"--read-only", "--model", "custom", "--max-steps=bad"}, "--max-steps expects an integer"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			code := Main(context.Background(), append([]string{"run"}, tc.args...), strings.NewReader(""), &stdout, &stderr)
			want := "error: " + tc.want + "\nreagent help run lists the flags\n"
			if code != exitUsage || stdout.Len() != 0 || stderr.String() != want {
				t.Fatalf("code %d, stdout %q, stderr %q; want %q", code, stdout.String(), stderr.String(), want)
			}
		})
	}
}

type interactionFailure struct{ err error }

func (f interactionFailure) Read([]byte) (int, error)  { return 0, f.err }
func (f interactionFailure) Write([]byte) (int, error) { return 0, f.err }

func TestReadPrompt_OwnedErrorsRetainCauses(t *testing.T) {
	failure := errors.New("library-specific detail")
	_, err := readPrompt("-", interactionFailure{failure})
	if !errors.Is(err, failure) || err.Error() != "read prompt: operation failed" {
		t.Fatalf("stdin error: %v", err)
	}
	_, err = readPrompt(filepath.Join(t.TempDir(), "missing"), nil)
	if !errors.Is(err, os.ErrNotExist) || err.Error() != "read prompt: path does not exist" {
		t.Fatalf("file error: %v", err)
	}
	_, err = readPrompt("-", strings.NewReader("\n"))
	if err == nil || err.Error() != "- contains no prompt" {
		t.Fatalf("owned validation changed: %v", err)
	}
}

func TestMain_OwnedPromptAndStdoutErrors(t *testing.T) {
	failure := interactionFailure{errors.New("library-specific detail")}
	var stderr bytes.Buffer
	code := Main(context.Background(), []string{"run", "--prompt-file", "-"}, failure, io.Discard, &stderr)
	if code != exitUsage || stderr.String() != "error: read prompt: operation failed\n" {
		t.Fatalf("prompt: code %d, stderr %q", code, stderr.String())
	}
	stderr.Reset()
	code = Main(context.Background(), []string{"run", "--workspace", t.TempDir(), "--provider", "openai", "--show-context", "hello"}, strings.NewReader(""), failure, &stderr)
	if code != exitRunFail || stderr.String() != "error: write stdout: operation failed\n" {
		t.Fatalf("stdout: code %d, stderr %q", code, stderr.String())
	}
}

func TestComposeInEditor_OwnedErrorsRetainCauses(t *testing.T) {
	_, err := composeInEditor([]string{filepath.Join(t.TempDir(), "missing")}, nil, nil, nil)
	if !errors.Is(err, os.ErrNotExist) || err.Error() != "run editor: path does not exist" {
		t.Fatalf("launch: %v", err)
	}
	_, err = composeInEditor([]string{"/bin/sh", "-c", "exit 7"}, nil, nil, nil)
	var exit *exec.ExitError
	if !errors.As(err, &exit) || err.Error() != "run editor: command exited with status 7" {
		t.Fatalf("exit: %v", err)
	}
	_, err = composeInEditor([]string{"/bin/sh", "-c", `rm "$1"`, "editor"}, nil, nil, nil)
	if !errors.Is(err, os.ErrNotExist) || err.Error() != "read editor file: path does not exist" {
		t.Fatalf("read: %v", err)
	}
	_, err = composeInEditor(nil, nil, nil, nil)
	if err == nil || err.Error() != "no editor configured" {
		t.Fatalf("owned validation changed: %v", err)
	}
}

func TestShell_OwnedLaunchErrorRetainsCause(t *testing.T) {
	_, err := runShellCommand(context.Background(), filepath.Join(t.TempDir(), "missing"), "true", io.Discard)
	if !errors.Is(err, os.ErrNotExist) || err.Error() != "run shell command: path does not exist" {
		t.Fatalf("launch: %v", err)
	}
}

func TestLineReader_OwnedLibraryErrorsRetainCauses(t *testing.T) {
	failure := errors.New("library-specific detail")
	reader := newLineReader(interactionFailure{failure}, io.Discard, nil)
	_, err := reader.ReadLine()
	if !errors.Is(err, failure) || err.Error() != "read chat input: operation failed" {
		t.Fatalf("read: %v", err)
	}
	scanner := bufio.NewScanner(strings.NewReader(strings.Repeat("x", 32)))
	scanner.Buffer(make([]byte, 8), 8)
	_, err = (&scannerReader{scanner}).ReadLine()
	if !errors.Is(err, bufio.ErrTooLong) || err.Error() != "read chat input: operation failed" {
		t.Fatalf("limit: %v", err)
	}
	_, err = newLineReader(strings.NewReader(""), io.Discard, nil).ReadLine()
	if err != io.EOF {
		t.Fatalf("EOF changed: %v", err)
	}
}

func TestTerminalInput_OwnedLibraryErrorsRetainCauses(t *testing.T) {
	reader := &terminalReader{fd: -1}
	_, err := reader.ReadLine()
	if !errors.Is(err, syscall.EBADF) || err.Error() != "enter terminal raw mode: operation failed" {
		t.Fatalf("raw mode: %v", err)
	}
	for _, read := range []func() error{
		func() error { return discardTerminalInput(-1) },
		func() error { _, err := (pollingReader{fd: -1}).Read(make([]byte, 1)); return err },
	} {
		if err := read(); !errors.Is(err, syscall.EBADF) || err.Error() != "read terminal flags: operation failed" {
			t.Fatalf("terminal flags: %v", err)
		}
	}
	file, err := os.Open(os.DevNull)
	if err != nil {
		t.Fatal(err)
	}
	file.Close()
	_, err = (terminalFileReader{file}).Read(make([]byte, 1))
	if !errors.Is(err, os.ErrClosed) || err.Error() != "read terminal: operation failed" {
		t.Fatalf("terminal read: %v", err)
	}
}

func TestTrace_OwnedLibraryWarnings(t *testing.T) {
	path := filepath.Join(t.TempDir(), "events.jsonl")
	if err := os.WriteFile(path, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	var warning bytes.Buffer
	trace := OpenTrace(path, "session", "run", &warning)
	if trace.Path() != "" || warning.String() != "warning: tracing disabled, this run is not recorded: path already exists\n" {
		t.Fatalf("open: %q", warning.String())
	}
	warning.Reset()
	trace.Open("session", "next", filepath.Join(t.TempDir(), "events.jsonl"))
	trace.Write("unsupported", 1, make(chan int))
	if trace.Path() != "" || warning.String() != "warning: tracing disabled, this run is not recorded: operation failed\n" {
		t.Fatalf("encode: %q", warning.String())
	}
}

func TestTracePath_OwnedCacheError(t *testing.T) {
	t.Setenv("HOME", "")
	t.Setenv("XDG_CACHE_HOME", "")
	for _, path := range []func() (string, error){
		func() (string, error) { return DefaultTracePath("run") },
		func() (string, error) { return tracePathFor("", "run") },
	} {
		if _, err := path(); err == nil || err.Error() != "locate trace cache: operation failed" || errors.Unwrap(err) == nil {
			t.Fatalf("cache: %v", err)
		}
	}
	want := filepath.Join("chosen", "run", "events.jsonl")
	if path, err := tracePathFor("chosen", "run"); err != nil || path != want {
		t.Fatalf("explicit directory: %q, %v", path, err)
	}
}
