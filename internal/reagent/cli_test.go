package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"flag"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestRun_ReportFrictionPreview(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		for _, readOnly := range []bool{false, true} {
			name := provider + "/default"
			if readOnly {
				name = provider + "/read_only"
			}
			t.Run(name, func(t *testing.T) {
				root := t.TempDir()
				var plain map[string]any
				for _, enabled := range []bool{false, true} {
					args := []string{"run", "--workspace", root, "--provider", provider, "--show-context"}
					if readOnly {
						args = append(args, "--read-only")
					}
					if enabled {
						args = append(args, "--report-friction")
					}
					args = append(args, "baseline")
					var out, errs bytes.Buffer
					if code := Main(context.Background(), args, strings.NewReader(""), &out, &errs); code != exitOK {
						t.Fatalf("exit %d: %s", code, errs.String())
					}
					var body map[string]any
					if err := json.Unmarshal(out.Bytes(), &body); err != nil {
						t.Fatal(err)
					}
					if !enabled {
						plain = body
						continue
					}
					var tools []any
					found := 0
					for _, tool := range body["tools"].([]any) {
						if tool.(map[string]any)["name"] == "report_friction" {
							found++
							continue
						}
						tools = append(tools, tool)
					}
					if found != 1 {
						t.Fatalf("report tool count %d", found)
					}
					body["tools"] = tools
					if provider == openaiName {
						text := body["instructions"].(string)
						if strings.Count(text, frictionInstructions) != 1 {
							t.Fatal("missing or repeated friction instructions")
						}
						body["instructions"] = strings.Replace(text, frictionInstructions, "", 1)
					} else {
						part := body["system"].([]any)[0].(map[string]any)
						text := part["text"].(string)
						if strings.Count(text, frictionInstructions) != 1 {
							t.Fatal("missing or repeated friction instructions")
						}
						part["text"] = strings.Replace(text, frictionInstructions, "", 1)
					}
					got, _ := json.Marshal(body)
					want, _ := json.Marshal(plain)
					if !bytes.Equal(got, want) {
						t.Fatalf("preview changed beyond the report tool/instructions:\n%s\n%s", got, want)
					}
				}
			})
		}
	}
}

func TestMain_ReportFrictionLaunchAndHelp(t *testing.T) {
	responses, err := json.Marshal([]ModelResponse{
		turn(callBlock("report", "report_friction", `{"category":"other","summary":"rough edge"}`)),
		turn(textBlock("done")), turn(textBlock("review")),
	})
	if err != nil {
		t.Fatal(err)
	}
	script := filepath.Join(t.TempDir(), "responses.json")
	if err := os.WriteFile(script, responses, 0o600); err != nil {
		t.Fatal(err)
	}
	for _, command := range []string{"run", "chat"} {
		var out, errs bytes.Buffer
		if code := Main(context.Background(), []string{"help", command}, strings.NewReader(""), &out, &errs); code != exitOK || !strings.Contains(out.String(), "--report-friction") {
			t.Fatalf("help: %s, %s", out.String(), errs.String())
		}
		out.Reset()
		errs.Reset()
		dir := t.TempDir()
		args := []string{command, "--workspace", t.TempDir(), "--read-only", "--report-friction", "--scripted", script}
		input := ""
		if command == "run" {
			args = append(args, "--trace-file", filepath.Join(dir, "events.jsonl"), "task")
		} else {
			args = append(args, "--trace-dir", dir)
			input = "task\n/friction\n/exit\n"
		}
		if code := Main(context.Background(), args, strings.NewReader(input), &out, &errs); code != exitOK || !strings.Contains(errs.String(), "✓ report_friction other: rough edge") {
			t.Fatalf("exit %d: %s", code, errs.String())
		}
		if command == "chat" && out.String() != "done\nreview\n" {
			t.Fatalf("chat replies %q", out.String())
		}
	}
}

func TestRun_PlanFlag(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		t.Run(provider, func(t *testing.T) {
			var out, errs bytes.Buffer
			code := Main(context.Background(), []string{"run", "--workspace", t.TempDir(), "--provider", provider, "--plan", "--show-context", "plan this"}, strings.NewReader(""), &out, &errs)
			if code != exitOK {
				t.Fatalf("exit %d: %s", code, errs.String())
			}
			if !strings.Contains(out.String(), "re:agent plan mode is on for this message") || !strings.Contains(out.String(), "plan this") || strings.Contains(out.String(), `"plan":"on"`) {
				t.Fatalf("request: %s", out.String())
			}
		})
	}
}

func TestRun_PlanBlockPrintsTheOriginalReply(t *testing.T) {
	text := "<plan>\n- Fix the parser\n</plan>"
	script, err := json.Marshal([]ModelResponse{turn(textBlock(text))})
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "responses.json")
	if err := os.WriteFile(path, script, 0o600); err != nil {
		t.Fatal(err)
	}
	var out, errs bytes.Buffer
	code := Main(context.Background(), []string{"run", "--workspace", t.TempDir(), "--plan", "--scripted", path, "plan"}, strings.NewReader(""), &out, &errs)
	if code != exitOK || out.String() != text+"\n" {
		t.Fatalf("exit %d; reply %q; stderr %s", code, out.String(), errs.String())
	}
}

func TestChat_PlanFlagStartsInPlanMode(t *testing.T) {
	var out, errs bytes.Buffer
	root, traceDir := t.TempDir(), t.TempDir()
	code := Main(context.Background(), []string{"chat", "--workspace", root, "--trace-dir", traceDir, "--plan", "--scripted", "../../testdata/scripts/echo_then_answer.json"}, strings.NewReader("question\n/exit\n"), &out, &errs)
	if code != exitOK || !strings.Contains(errs.String(), "plan mode") {
		t.Fatalf("exit %d; stderr %s", code, errs.String())
	}
	paths, err := filepath.Glob(filepath.Join(traceDir, "*", "events.jsonl"))
	if err != nil || len(paths) != 1 {
		t.Fatalf("traces: %q; %v", paths, err)
	}
	var request ModelRequest
	for _, event := range readEvents(t, paths[0]) {
		if event.Type == "model.requested" {
			data, _ := json.Marshal(event.Data)
			if err := json.Unmarshal(data, &request); err != nil {
				t.Fatal(err)
			}
			break
		}
	}
	if len(request.History) == 0 || request.History[0].User.Plan != "on" {
		t.Fatalf("request: %+v", request)
	}
}

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
	if !strings.Contains(stderr.String(), "completed ·") || !strings.Contains(stderr.String(), "trace: "+trace) {
		t.Fatalf("summary or trace path missing: %q", stderr.String())
	}
	if strings.Contains(stderr.String(), "  ran ") || strings.Contains(stderr.String(), "  changed ") {
		t.Fatalf("completed run unexpectedly printed a recap: %q", stderr.String())
	}
	if !strings.Contains(stderr.String(), "✓ echo") {
		t.Fatalf("default output lost live tool activity: %q", stderr.String())
	}
}

func writeEditExecScript(t *testing.T, root string, complete bool) string {
	t.Helper()
	if err := os.WriteFile(filepath.Join(root, "a.txt"), []byte("before\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	ws, err := OpenWorkspace(root)
	if err != nil {
		t.Fatal(err)
	}
	editArgs, err := json.Marshal(editFileArgs{
		Path: "a.txt", ExpectedSHA256: digestOfFile(t, ws, "a.txt"), OldText: "before", NewText: "after",
	})
	if err != nil {
		t.Fatal(err)
	}
	execArgsJSON, err := json.Marshal(execArgs{
		Argv: []string{"true"}, Cwd: ".", TimeoutMS: json.RawMessage("1000"),
	})
	if err != nil {
		t.Fatal(err)
	}
	responses := []ModelResponse{turn(
		callBlock("call_edit", "edit_file", string(editArgs)),
		callBlock("call_exec", "exec", string(execArgsJSON)),
	)}
	if complete {
		responses = append(responses, turn(textBlock("finished")))
	}
	script, err := json.Marshal(responses)
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "responses.json")
	if err := os.WriteFile(path, script, 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func recapRunArgs(root, script, trace string, recap bool) []string {
	args := []string{"run", "--workspace", root,
		"--scripted", script, "--trace-file", trace}
	if recap {
		args = append(args, "--recap")
	}
	return append(args, "apply the requested changes")
}

func TestMain_CompletedRunRecapIsOptIn(t *testing.T) {
	for _, recap := range []bool{false, true} {
		name := "default"
		if recap {
			name = "enabled"
		}
		t.Run(name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			root := t.TempDir()
			script := writeEditExecScript(t, root, true)
			trace := filepath.Join(t.TempDir(), "events.jsonl")
			args := recapRunArgs(root, script, trace, recap)
			code := Main(context.Background(), args, strings.NewReader(""), &stdout, &stderr)
			if code != exitOK {
				t.Fatalf("exit %d, stderr: %s", code, stderr.String())
			}
			if !strings.Contains(stderr.String(), "completed · 2 steps · 2 tool calls") || !strings.Contains(stderr.String(), "trace: "+trace) {
				t.Fatalf("summary or trace path missing: %q", stderr.String())
			}
			changed := strings.Contains(stderr.String(), "  changed a.txt\n")
			ran := strings.Contains(stderr.String(), "  ran     true\n")
			if changed != recap || ran != recap {
				t.Fatalf("recap present changed=%t ran=%t, requested %t: %q", changed, ran, recap, stderr.String())
			}
		})
	}
}

func TestMain_NoncompletedRunAlwaysShowsRecap(t *testing.T) {
	var stdout, stderr bytes.Buffer
	root := t.TempDir()
	script := writeEditExecScript(t, root, false)
	trace := filepath.Join(t.TempDir(), "events.jsonl")
	args := []string{"run", "--workspace", root,
		"--scripted", script, "--trace-file", trace, "attempt the changes"}

	if code := Main(context.Background(), args, strings.NewReader(""), &stdout, &stderr); code != exitRunFail {
		t.Fatalf("exit %d, want %d; stderr: %s", code, exitRunFail, stderr.String())
	}
	for _, want := range []string{
		"protocol_error · 2 steps · 2 tool calls", "script exhausted after 1 responses",
		"  changed a.txt\n", "  ran     true\n", "trace: " + trace,
	} {
		if !strings.Contains(stderr.String(), want) {
			t.Fatalf("stderr lacks %q:\n%s", want, stderr.String())
		}
	}
	if got, err := os.ReadFile(filepath.Join(root, "a.txt")); err != nil || string(got) != "after\n" {
		t.Fatalf("edited file = %q, error %v", got, err)
	}
}

func TestMain_ChatRecapIsOptIn(t *testing.T) {
	for _, recap := range []bool{false, true} {
		name := "default"
		if recap {
			name = "enabled"
		}
		t.Run(name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			root := t.TempDir()
			script := writeEditExecScript(t, root, true)
			args := []string{"chat", "--workspace", root,
				"--scripted", script, "--trace-dir", filepath.Join(t.TempDir(), "traces")}
			if recap {
				args = append(args, "--recap")
			}

			code := Main(context.Background(), args, strings.NewReader("apply the requested changes\n"), &stdout, &stderr)
			if code != exitOK {
				t.Fatalf("exit %d, stderr: %s", code, stderr.String())
			}
			if got := strings.TrimSpace(stdout.String()); got != "finished" {
				t.Fatalf("stdout: %q", got)
			}
			if !strings.Contains(stderr.String(), "completed · 2 steps · 2 tool calls") {
				t.Fatalf("completed status missing: %q", stderr.String())
			}
			changed := strings.Contains(stderr.String(), "  changed a.txt\n")
			ran := strings.Contains(stderr.String(), "  ran     true\n")
			if changed != recap || ran != recap {
				t.Fatalf("recap present changed=%t ran=%t, requested %t: %q", changed, ran, recap, stderr.String())
			}
		})
	}
}

func TestMain_FlagAfterPromptIsRefused(t *testing.T) {
	for flag, want := range map[string]string{
		"--read-only":      "--read-only",
		"--read-only=true": "--read-only",
		"-model=x":         "--model",
		"--help":           "--help",
	} {
		t.Run(flag, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			code := Main(context.Background(), []string{"run", "--show-context", "a task", flag}, strings.NewReader(""), &stdout, &stderr)
			if code != exitUsage {
				t.Fatalf("exit %d, want %d", code, exitUsage)
			}
			if !strings.Contains(stderr.String(), want) {
				t.Fatalf("stderr %q does not name %q", stderr.String(), want)
			}
		})
	}
}

func TestMain_DoubleDashKeepsFlagLikeTextInThePrompt(t *testing.T) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run", "--show-context", "--", "--allow-write"}, strings.NewReader(""), &stdout, &stderr)
	if code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}
	if !strings.Contains(stdout.String(), "--allow-write") {
		t.Fatalf("preview does not contain prompt: %s", stdout.String())
	}
}

func TestMain_DoubleDashAfterPromptIsPromptText(t *testing.T) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run", "--show-context", "explain", "--", "--allow-write"}, strings.NewReader(""), &stdout, &stderr)
	if code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}
	if !strings.Contains(stdout.String(), "explain -- --allow-write") {
		t.Fatalf("preview does not contain the prompt: %s", stdout.String())
	}
}

func TestMain_UnknownFlagStaysOffStdout(t *testing.T) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run", "--bogus", "a task"}, strings.NewReader(""), &stdout, &stderr)
	if code != exitUsage {
		t.Fatalf("exit %d, want %d", code, exitUsage)
	}
	if stdout.Len() != 0 || !strings.Contains(stderr.String(), "flag provided but not defined") {
		t.Fatalf("stdout %q, stderr %q", stdout.String(), stderr.String())
	}
}

func TestMain_ChatPromptDoesNotReportAMisplacedFlag(t *testing.T) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"chat", "a prompt", "--allow-write"}, strings.NewReader(""), &stdout, &stderr)
	if code != exitUsage {
		t.Fatalf("exit %d, want %d", code, exitUsage)
	}
	if !strings.Contains(stderr.String(), "chat reads its turns from stdin and takes no prompt") {
		t.Fatalf("stderr: %q", stderr.String())
	}
}

func TestMain_ChatUsesDefaultPerTurnBudgets(t *testing.T) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"chat", "--scripted", "../../testdata/scripts/echo_then_answer.json"},
		strings.NewReader("/status\n"), &stdout, &stderr)
	if code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}
	if !strings.Contains(stderr.String(), "budget     unlimited steps, unlimited tool calls per turn") {
		t.Fatalf("chat budget: %s", stderr.String())
	}
}

func TestMain_HelpGoesToStdoutAndExitsZero(t *testing.T) {
	for _, args := range [][]string{{"-h"}, {"--help"}, {"help"}, {"help", "run"}, {"help", "chat"}, {"run", "-h"}, {"chat", "--help"}} {
		t.Run(strings.Join(args, " "), func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			code := Main(context.Background(), args, strings.NewReader(""), &stdout, &stderr)
			if code != exitOK || stdout.Len() == 0 || stderr.Len() != 0 {
				t.Fatalf("exit %d, stdout %q, stderr %q", code, stdout.String(), stderr.String())
			}
		})
	}
}

func TestHelp_ListsEachCommandsOwnFlags(t *testing.T) {
	for _, command := range []string{"run", "chat"} {
		t.Run(command, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			if code := Main(context.Background(), []string{"help", command}, strings.NewReader(""), &stdout, &stderr); code != exitOK {
				t.Fatalf("exit %d, stderr: %s", code, stderr.String())
			}
			if !strings.Contains(stdout.String(), "--no-project-instructions") {
				t.Fatalf("project instruction flag missing from %s help: %s", command, stdout.String())
			}
			if command == "run" && (!strings.Contains(stdout.String(), "--prompt-file") || strings.Contains(stdout.String(), "--trace-dir")) {
				t.Fatalf("run help: %s", stdout.String())
			}
			if command == "chat" && (!strings.Contains(stdout.String(), "--trace-dir") || strings.Contains(stdout.String(), "--prompt-file")) {
				t.Fatalf("chat help: %s", stdout.String())
			}
		})
	}
}

func TestHelp_EveryFlagIsInAGroup(t *testing.T) {
	fs := flag.NewFlagSet("test", flag.ContinueOnError)
	defineFlags(fs)
	listed := map[string]bool{}
	for _, groups := range [][]flagGroup{runFlagGroups, chatFlagGroups} {
		for _, group := range groups {
			for _, name := range group.flags {
				listed[name] = true
			}
		}
	}
	fs.VisitAll(func(f *flag.Flag) {
		if !listed[f.Name] {
			t.Errorf("%s is missing from command help", f.Name)
		}
	})
}

func TestMain_Version(t *testing.T) {
	var stdout, stderr bytes.Buffer
	if code := Main(context.Background(), []string{"version"}, strings.NewReader(""), &stdout, &stderr); code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}
	if !strings.HasPrefix(stdout.String(), "reagent ") || !strings.Contains(stdout.String(), runtime.Version()) {
		t.Fatalf("version: %q", stdout.String())
	}
}

func TestMain_BareCommandIsAUsageError(t *testing.T) {
	var stdout, stderr bytes.Buffer
	if code := Main(context.Background(), nil, strings.NewReader(""), &stdout, &stderr); code != exitUsage {
		t.Fatalf("exit %d, want %d", code, exitUsage)
	}
	if stdout.Len() != 0 || stderr.Len() == 0 {
		t.Fatalf("stdout %q, stderr %q", stdout.String(), stderr.String())
	}
}

func TestMain_UnknownCommandExplainsTheError(t *testing.T) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"frobnicate"}, strings.NewReader(""), &stdout, &stderr)
	if code != exitUsage {
		t.Fatalf("exit %d, want %d", code, exitUsage)
	}
	if stdout.Len() != 0 || !strings.Contains(stderr.String(), "error: unknown command frobnicate") {
		t.Fatalf("stdout %q, stderr %q", stdout.String(), stderr.String())
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

func TestMain_ProjectInstructionsAtLaunch(t *testing.T) {
	for _, tc := range []struct {
		name, warning           string
		content                 []byte
		create, skip, directory bool
	}{
		{name: "missing"},
		{name: "valid", create: true, content: []byte("project-only\n")},
		{name: "empty is present", create: true},
		{name: "exactly 32 KiB", create: true, content: bytes.Repeat([]byte("x"), 32<<10)},
		{name: "too large", create: true, content: bytes.Repeat([]byte("x"), (32<<10)+1), warning: "larger than 32 KiB"},
		{name: "directory", directory: true, warning: "not a regular file"},
		{name: "invalid UTF-8", create: true, content: []byte{0xff}, warning: "not UTF-8"},
		{name: "disabled", create: true, content: []byte("project-only\n"), skip: true},
		{name: "disabled invalid file", create: true, content: []byte{0xff}, skip: true},
	} {
		for _, provider := range []string{openaiName, anthropicName} {
			t.Run(tc.name+"/"+provider, func(t *testing.T) {
				root := t.TempDir()
				if tc.create {
					if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), tc.content, 0o600); err != nil {
						t.Fatal(err)
					}
				}
				if tc.directory {
					if err := os.Mkdir(filepath.Join(root, "AGENTS.md"), 0o700); err != nil {
						t.Fatal(err)
					}
				}
				args := []string{"run", "--workspace", root, "--provider", provider, "--show-context"}
				if tc.skip {
					args = append(args, "--no-project-instructions")
				}
				args = append(args, "task")
				var stdout, stderr bytes.Buffer
				if code := Main(context.Background(), args, strings.NewReader(""), &stdout, &stderr); code != exitOK {
					t.Fatalf("exit %d: %s", code, stderr.String())
				}
				var request struct {
					Instructions string              `json:"instructions"`
					System       []messagesTextBlock `json:"system"`
				}
				if err := json.Unmarshal(stdout.Bytes(), &request); err != nil {
					t.Fatal(err)
				}
				text := request.Instructions
				if provider == anthropicName {
					text = request.System[0].Text
				}
				loaded := tc.create && !tc.skip && tc.warning == ""
				if strings.Contains(text, "# Project instructions (AGENTS.md)") != loaded {
					t.Fatalf("loaded=%t, instructions tail: %q", loaded, text[len(text)-min(len(text), 100):])
				}
				if loaded && !strings.HasSuffix(text, "\n# Project instructions (AGENTS.md)\n\n"+string(tc.content)) {
					t.Fatalf("project instructions were changed: %q", text[len(text)-min(len(text), 100):])
				}
				if tc.warning == "" && stderr.Len() != 0 || tc.warning != "" && (!strings.Contains(stderr.String(), tc.warning) || strings.Count(stderr.String(), "\n") != 1) {
					t.Fatalf("warning: %q", stderr.String())
				}
			})
		}
	}
}

func TestMain_ProjectInstructionsOnlyFromRoot(t *testing.T) {
	parent := t.TempDir()
	root := filepath.Join(parent, "child")
	if err := os.Mkdir(root, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(filepath.Join(root, "nested"), 0o700); err != nil {
		t.Fatal(err)
	}
	for _, path := range []string{filepath.Join(parent, "AGENTS.md"), filepath.Join(root, "nested", "AGENTS.md")} {
		if err := os.WriteFile(path, []byte("outside the root\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	var stdout, stderr bytes.Buffer
	if code := Main(context.Background(), []string{"run", "--workspace", root, "--show-context", "task"}, strings.NewReader(""), &stdout, &stderr); code != exitOK || stderr.Len() != 0 {
		t.Fatalf("exit %d, stderr %q", code, stderr.String())
	}
	if strings.Contains(stdout.String(), "Project instructions (AGENTS.md)") || strings.Contains(stdout.String(), "outside the root") {
		t.Fatalf("read a parent or nested AGENTS.md: %s", stdout.String())
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
	registry, err := NewRegistry(Mode{}, NewListFilesTool(ws), NewReadFileTool(ws), NewSearchTextTool(ws), NewEditFileTool(ws), NewWriteFileTool(ws), NewDeleteFileTool(ws), NewExecTool(ws), NewRequestWorkspaceAccessTool(), NewSwitchWorkspaceTool())
	if err != nil {
		t.Fatal(err)
	}
	want, err := PreviewRequest(Config{
		Provider: openaiName, Model: DefaultOpenAIModel, ReasoningEffort: DefaultReasoningEffort,
		Registry: registry, WorkspacePath: ws.Root(),
	}, "Where is the timeout set?", collectSnapshot(context.Background(), ws.Root(), false))
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

// The preview shows all tools by default and only read tools in read-only mode.
func TestMain_ReadOnlyWithholdsMutatingTools(t *testing.T) {
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
		mode := request.Instructions[strings.Index(request.Instructions, "Mode: "):]
		return strings.Join(names, ",") + " | " + strings.SplitN(mode, "\n", 2)[0]
	}

	if got := declared(); got != "delete_file,edit_file,exec,list_files,read_file,request_workspace_access,search_text,switch_workspace,write_file | Mode: read, write, and execute" {
		t.Fatalf("default run declared %q", got)
	}
	if got := declared("--read-only"); got != "list_files,read_file,request_workspace_access,search_text,switch_workspace | Mode: read only" {
		t.Fatalf("read-only run declared %q", got)
	}
}

// An issue report is long and multi-line, so a run takes its prompt from a
// file with everything but the trailing newline preserved.
func TestMain_PromptFile(t *testing.T) {
	issue := "Title: crash on empty input\n\nSteps:\n  1. call parse(\"\")\n  2. observe the panic\n"
	path := filepath.Join(t.TempDir(), "issue.txt")
	if err := os.WriteFile(path, []byte(issue), 0o600); err != nil {
		t.Fatal(err)
	}

	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run",
		"--workspace", t.TempDir(), "--show-context", "--prompt-file", path},
		strings.NewReader(""), &stdout, &stderr)
	if code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}

	var request decodedRequest
	if err := json.Unmarshal(bytes.TrimSpace(stdout.Bytes()), &request); err != nil {
		t.Fatal(err)
	}
	var message responsesMessage
	if err := json.Unmarshal(request.Input[0], &message); err != nil {
		t.Fatal(err)
	}
	// Only the final newline is dropped; the blank line and indentation stay.
	if want := strings.TrimSuffix(issue, "\n"); message.Content[0].Text != want {
		t.Fatalf("got %q, want %q", message.Content[0].Text, want)
	}
}

func TestMain_PromptFileFromStdin(t *testing.T) {
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run",
		"--workspace", t.TempDir(), "--show-context", "--prompt-file", "-"},
		strings.NewReader("piped issue text\n"), &stdout, &stderr)
	if code != exitOK {
		t.Fatalf("exit %d, stderr: %s", code, stderr.String())
	}
	if !strings.Contains(stdout.String(), "piped issue text") {
		t.Fatalf("stdout: %s", stdout.String())
	}
}

func TestMain_PromptSourceErrors(t *testing.T) {
	path := filepath.Join(t.TempDir(), "empty.txt")
	if err := os.WriteFile(path, []byte("  \n"), 0o600); err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	cases := map[string][]string{
		"both sources":       {"run", "--workspace", dir, "--prompt-file", path, "a prompt"},
		"neither":            {"run", "--workspace", dir},
		"missing file":       {"run", "--workspace", dir, "--prompt-file", "/nonexistent/issue.txt"},
		"blank file":         {"run", "--workspace", dir, "--prompt-file", path},
		"chat takes neither": {"chat", "--workspace", dir, "--prompt-file", path},
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
