package reagent

import (
	"context"
	"encoding/json"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
)

func TestReportFriction_ValidatesArguments(t *testing.T) {
	args := func(summary, details string, ids int) string {
		raw, err := json.Marshal(map[string]any{
			"category": "other", "summary": summary, "details": details,
			"related_call_ids": make([]string, ids),
		})
		if err != nil {
			t.Fatal(err)
		}
		return string(raw)
	}
	tests := []struct {
		name, args string
		valid      bool
	}{
		{"minimal", `{"category":"other","summary":"A rough edge"}`, true},
		{"at limits", args(strings.Repeat("界", 300), strings.Repeat("界", 4000), 20), true},
		{"empty optionals", args("summary", "", 0), true},
		{"multiline details", args("summary", "line one\nline two", 1), true},
		{"unknown category", `{"category":"oops","summary":"summary"}`, false},
		{"missing category", `{"summary":"summary"}`, false},
		{"missing summary", `{"category":"other"}`, false},
		{"empty summary", args("", "", 0), false},
		{"blank summary", args(" \t ", "", 0), false},
		{"newline", args("one\ntwo", "", 0), false},
		{"carriage return", args("one\rtwo", "", 0), false},
		{"long summary", args(strings.Repeat("界", 301), "", 0), false},
		{"long details", args("summary", strings.Repeat("界", 4001), 0), false},
		{"too many ids", args("summary", "", 21), false},
		{"invalid json", `{`, false},
		{"array", `[]`, false},
		{"trailing object", `{"category":"other","summary":"summary"} {}`, false},
		{"unknown field", `{"category":"other","summary":"summary","extra":1}`, false},
		{"wrong category type", `{"category":1,"summary":"summary"}`, false},
		{"wrong summary type", `{"category":"other","summary":1}`, false},
		{"null category", `{"category":null,"summary":"summary"}`, false},
		{"null summary", `{"category":"other","summary":null}`, false},
		{"null details", `{"category":"other","summary":"summary","details":null}`, false},
		{"wrong details type", `{"category":"other","summary":"summary","details":1}`, false},
		{"null ids", `{"category":"other","summary":"summary","related_call_ids":null}`, false},
		{"wrong ids type", `{"category":"other","summary":"summary","related_call_ids":"id"}`, false},
		{"wrong id type", `{"category":"other","summary":"summary","related_call_ids":[1]}`, false},
		{"null id", `{"category":"other","summary":"summary","related_call_ids":[null]}`, false},
	}
	for _, category := range []string{"misleading_error", "missing_capability", "unclear_description", "harness_bug", "other"} {
		tests = append(tests, struct {
			name, args string
			valid      bool
		}{category, `{"category":"` + category + `","summary":"summary"}`, true})
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			out, err := NewReportFrictionTool().Execute(context.Background(), json.RawMessage(test.args))
			if err != nil || out.OK != test.valid || out.Effect != EffectNone {
				t.Fatalf("outcome %+v, error %v", out, err)
			}
			if !test.valid && out.Code != "invalid_arguments" {
				t.Fatalf("code %q", out.Code)
			}
			if test.valid && string(out.Data) != `{"recorded":true}` {
				t.Fatalf("data %s", out.Data)
			}
		})
	}
}

func TestReportFriction_HasNoEffectAndWorksReadOnly(t *testing.T) {
	tool := NewReportFrictionTool()
	r, err := NewRegistry(Mode{ReadOnly: true}, tool)
	if err != nil {
		t.Fatal(err)
	}
	if found, ok := r.Lookup("report_friction"); !ok || found != tool || len(r.Specs()) != 1 || r.Specs()[0].Effect != EffectClassRead {
		t.Fatalf("registry %+v", r)
	}
	var schema struct {
		Required   []string `json:"required"`
		Additional bool     `json:"additionalProperties"`
	}
	if err := json.Unmarshal(tool.Spec().InputSchema, &schema); err != nil || strings.Join(schema.Required, ",") != "category,summary" || schema.Additional {
		t.Fatalf("schema %+v, error %v", schema, err)
	}
	out, err := tool.Execute(context.Background(), json.RawMessage(`{"category":"other","summary":"summary"}`))
	if err != nil || !out.OK || out.Effect != EffectNone {
		t.Fatalf("outcome %+v, error %v", out, err)
	}
}

func TestReportFriction_StopsAfterTheCap(t *testing.T) {
	tool := NewReportFrictionTool()
	for i := 0; i < 10; i++ {
		invalid, err := tool.Execute(context.Background(), json.RawMessage(`{}`))
		if err != nil || invalid.OK {
			t.Fatalf("invalid outcome %+v, error %v", invalid, err)
		}
		out, err := tool.Execute(context.Background(), json.RawMessage(`{"category":"other","summary":"summary"}`))
		if err != nil || !out.OK {
			t.Fatalf("report %d: %+v, error %v", i+1, out, err)
		}
	}
	out, err := tool.Execute(context.Background(), json.RawMessage(`{"category":"other","summary":"summary"}`))
	if err != nil || out.Code != "invalid_arguments" || out.Message != "report limit reached; carry on with the task" || out.Effect != EffectNone {
		t.Fatalf("eleventh report: %+v, error %v", out, err)
	}
}

func TestReportFriction_ConcurrentCap(t *testing.T) {
	tool := NewReportFrictionTool()
	var successes atomic.Int32
	var wg sync.WaitGroup
	for i := 0; i < 30; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			out, err := tool.Execute(context.Background(), json.RawMessage(`{"category":"other","summary":"summary"}`))
			if err != nil {
				t.Error(err)
			}
			if out.OK {
				successes.Add(1)
			}
		}()
	}
	wg.Wait()
	if got := successes.Load(); got != 10 {
		t.Fatalf("%d successful reports", got)
	}
}
