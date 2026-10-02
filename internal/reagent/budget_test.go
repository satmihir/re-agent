package reagent

import (
	"bytes"
	"context"
	"fmt"
	"strings"
	"testing"
)

func TestLoop_UnlimitedBudgetsAreIndependent(t *testing.T) {
	for _, test := range []struct {
		name         string
		steps, calls int
		want         RunStatus
		wantRuns     int
	}{
		{"both unlimited", 0, 0, StatusCompleted, 402},
		{"only steps unlimited", 0, 401, StatusLimitExceeded, 400},
		{"only calls unlimited", 201, 0, StatusLimitExceeded, 400},
		{"finite steps enough", 202, 0, StatusCompleted, 402},
		{"finite calls enough", 0, 402, StatusCompleted, 402},
	} {
		t.Run(test.name, func(t *testing.T) {
			runs := 0
			cfg := testConfig(t, countingTool{runs: &runs})
			cfg.MaxSteps, cfg.MaxToolCalls = test.steps, test.calls
			var responses []ModelResponse
			for i := 0; i < 201; i++ {
				responses = append(responses, turn(
					callBlock(fmt.Sprintf("call_%d_a", i), "counter", `{}`),
					callBlock(fmt.Sprintf("call_%d_b", i), "counter", `{}`)))
			}
			responses = append(responses, turn(textBlock("done")))
			_, result := runScript(t, cfg, responses...)
			if result.Status != test.want || runs != test.wantRuns || result.ToolCalls != runs {
				t.Fatalf("result %+v, tool runs %d; want %s, %d runs", result, runs, test.want, test.wantRuns)
			}
			if test.want == StatusCompleted && result.Steps != 202 {
				t.Fatalf("steps %d, want 202", result.Steps)
			}
		})
	}
}

func TestMain_BudgetOptions(t *testing.T) {
	for _, command := range []string{"run", "chat"} {
		for _, test := range []struct {
			name         string
			steps, calls string
			want         int
		}{
			{"zero", "0", "0", exitOK},
			{"positive", "2", "1", exitOK},
			{"unlimited steps", "0", "1", exitOK},
			{"unlimited calls", "2", "0", exitOK},
			{"negative steps", "-1", "0", exitUsage},
			{"negative calls", "0", "-1", exitUsage},
		} {
			t.Run(command+"/"+test.name, func(t *testing.T) {
				args := []string{command, "--scripted", "../../testdata/scripts/echo_then_answer.json",
					"--max-steps", test.steps, "--max-tool-calls", test.calls}
				input := ""
				if command == "run" {
					args = append(args, "task")
				} else {
					args = append(args, "--trace-dir", t.TempDir())
					input = "task\n"
				}
				var stdout, stderr bytes.Buffer
				code := Main(context.Background(), args, strings.NewReader(input), &stdout, &stderr)
				if code != test.want {
					t.Fatalf("exit %d, want %d: %s", code, test.want, stderr.String())
				}
				if test.want == exitUsage && !strings.Contains(stderr.String(), "must be nonnegative") {
					t.Fatalf("stderr: %s", stderr.String())
				}
			})
		}
	}
}

func TestContext_UnlimitedBudgetText(t *testing.T) {
	for _, test := range []struct {
		steps, calls int
		want         string
	}{
		{0, 0, "unlimited model requests and unlimited tool calls"},
		{0, 7, "unlimited model requests and 7 tool calls"},
		{3, 0, "3 model requests and unlimited tool calls"},
	} {
		cfg := testConfig(t)
		cfg.MaxSteps, cfg.MaxToolCalls = test.steps, test.calls
		request := BuildContext(cfg, RequestScope{Step: 1}, nil)
		if !strings.Contains(request.Instructions, "Budget: "+test.want+" per run\n") {
			t.Fatalf("instructions: %s", request.Instructions)
		}
	}
}

func TestDisplay_UnlimitedStepHasNoDenominator(t *testing.T) {
	var output bytes.Buffer
	d := NewDisplay(&output)
	d.live = true // A buffer needs the terminal progress path enabled explicitly.
	d.modelStarted("test-model", 201, 0)
	d.mu.Lock()
	label := d.status.label
	d.mu.Unlock()
	d.modelFinished()
	if label != "waiting for test-model · step 201" {
		t.Fatalf("progress label: %q", label)
	}
}
