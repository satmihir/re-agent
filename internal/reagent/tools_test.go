package reagent

import (
	"context"
	"encoding/json"
	"testing"
)

type namedTool struct{ name string }

func (n namedTool) Spec() ToolSpec {
	return ToolSpec{Name: n.name, InputSchema: json.RawMessage(`{"type":"object"}`)}
}
func (namedTool) Execute(context.Context, json.RawMessage) (ToolOutcome, error) {
	return ToolOutcome{}, nil
}

func TestNewRegistry_RejectsBadNames(t *testing.T) {
	cases := map[string][]Tool{
		"duplicate": {namedTool{"echo"}, namedTool{"echo"}},
		"uppercase": {namedTool{"Echo"}},
		"empty":     {namedTool{""}},
		"dash":      {namedTool{"read-file"}},
	}
	for name, tools := range cases {
		t.Run(name, func(t *testing.T) {
			if _, err := NewRegistry(Mode{}, tools...); err == nil {
				t.Fatal("want an error")
			}
		})
	}
}

func TestEchoTool_ReturnsTextUnchanged(t *testing.T) {
	outcome, err := NewEchoTool().Execute(context.Background(), json.RawMessage(`{"text":"hi"}`))
	if err != nil {
		t.Fatal(err)
	}
	if !outcome.OK || outcome.Code != "ok" || outcome.Effect != EffectNone {
		t.Fatalf("got %+v", outcome)
	}
	if string(outcome.Data) != `{"text":"hi"}` {
		t.Fatalf("got data %s", outcome.Data)
	}
}

func TestDecodeArgs_RejectsTrailingTokens(t *testing.T) {
	var args echoArgs
	if bad := decodeArgs(json.RawMessage(`{"text":"a"}{"text":"b"}`), &args); bad == nil {
		t.Fatal("want an invalid_arguments outcome")
	}
}
