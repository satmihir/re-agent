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

func TestCLI_GitDoDefaultsAndOverrides(t *testing.T) {
	t.Setenv("TYPESAFE_API_KEY", "preview-only")
	for _, provider := range []string{openaiName, anthropicName} {
		for _, tc := range []struct {
			name, mode string
			flags      []string
		}{
			{"default", "recipe", nil},
			{"recipe", "recipe", []string{"--git-do=recipe"}},
			{"jev", "jev", []string{"--git-do=jev"}},
			{"off", "", []string{"--git-do=off"}},
			{"legacy_off", "", []string{"--git-do="}},
			{"read_only", "", []string{"--read-only"}},
			{"plan", "recipe", []string{"--plan"}},
		} {
			t.Run(provider+"/"+tc.name, func(t *testing.T) {
				args := []string{"run", "--workspace", t.TempDir(), "--provider", provider, "--show-context"}
				args = append(args, tc.flags...)
				args = append(args, "task")
				var out, errs bytes.Buffer
				if code := Main(context.Background(), args, strings.NewReader(""), &out, &errs); code != exitOK {
					t.Fatalf("exit %d: %s", code, errs.String())
				}
				var request struct {
					Tools []struct {
						Name        string         `json:"name"`
						Parameters  map[string]any `json:"parameters"`
						InputSchema map[string]any `json:"input_schema"`
					} `json:"tools"`
				}
				if err := json.Unmarshal(out.Bytes(), &request); err != nil {
					t.Fatal(err)
				}
				found := false
				for _, tool := range request.Tools {
					if tool.Name != "git_do" {
						continue
					}
					found = true
					schema := tool.Parameters
					if provider == anthropicName {
						schema = tool.InputSchema
					}
					want := "recipe"
					if tc.mode == "jev" {
						want = "intent"
					}
					required, _ := json.Marshal(schema["required"])
					if string(required) != `["`+want+`"]` {
						t.Fatalf("required %s; want %s", required, want)
					}
				}
				if found != (tc.mode != "") {
					t.Fatalf("git_do present: %t; mode %q", found, tc.mode)
				}
			})
		}
	}
}

func TestCLI_GitDoLaunchAndValidation(t *testing.T) {
	t.Setenv("TYPESAFE_API_KEY", "")
	script := filepath.Join(t.TempDir(), "responses.json")
	responses, err := json.Marshal([]ModelResponse{
		turn(callBlock("git", "git_do", `{}`)), turn(textBlock("done")),
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(script, responses, 0o600); err != nil {
		t.Fatal(err)
	}
	for _, command := range []string{"run", "chat"} {
		for _, tc := range []struct {
			flag, code string
		}{
			{"", "invalid_arguments"},
			{"--git-do=off", "tool_unavailable"},
			{"--plan", "plan_mode"},
		} {
			t.Run(command+"/"+tc.flag, func(t *testing.T) {
				traceDir := t.TempDir()
				args := []string{command, "--workspace", t.TempDir(), "--scripted", script}
				if tc.flag != "" {
					args = append(args, tc.flag)
				}
				input := "task\n/exit\n"
				trace := filepath.Join(traceDir, "events.jsonl")
				if command == "run" {
					args = append(args, "--trace-file", trace, "task")
					input = ""
				} else {
					args = append(args, "--trace-dir", traceDir)
				}
				var out, errs bytes.Buffer
				if code := Main(context.Background(), args, strings.NewReader(input), &out, &errs); code != exitOK {
					t.Fatalf("exit %d: %s", code, errs.String())
				}
				if command == "chat" {
					paths, _ := filepath.Glob(filepath.Join(traceDir, "*", "events.jsonl"))
					if len(paths) != 1 {
						t.Fatalf("traces: %v", paths)
					}
					trace = paths[0]
				}
				for _, event := range readEvents(t, trace) {
					if event.Type == "tool.finished" {
						data, _ := json.Marshal(event.Data)
						if !bytes.Contains(data, []byte(`"code":"`+tc.code+`"`)) {
							t.Fatalf("outcome: %s", data)
						}
						return
					}
				}
				t.Fatal("missing tool result")
			})
		}
		for _, tc := range []struct{ mode, message string }{
			{"jev", "--git-do jev needs TYPESAFE_API_KEY"},
			{"bogus", "--git-do must be recipe, jev or off"},
		} {
			args := []string{command, "--workspace", t.TempDir(), "--git-do", tc.mode}
			if command == "run" {
				args = append(args, "--show-context", "task")
			}
			var out, errs bytes.Buffer
			if code := Main(context.Background(), args, strings.NewReader(""), &out, &errs); code != exitUsage || !strings.Contains(errs.String(), tc.message) {
				t.Fatalf("%s %s: exit %d: %s", command, tc.mode, code, errs.String())
			}
		}
	}
}

func TestCLI_GitDoHelpNamesAllModes(t *testing.T) {
	for _, command := range []string{"run", "chat"} {
		var out, errs bytes.Buffer
		if code := Main(context.Background(), []string{"help", command}, strings.NewReader(""), &out, &errs); code != exitOK {
			t.Fatalf("exit %d: %s", code, errs.String())
		}
		if !strings.Contains(out.String(), "--git-do MODE") || !strings.Contains(out.String(), "or off (default recipe)") {
			t.Fatalf("help: %s", out.String())
		}
	}
}
