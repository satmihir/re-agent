package reagent

import (
	"strings"
	"testing"
)

func TestBuildContext_IsPureAndOrdered(t *testing.T) {
	cfg := testConfig(t, NewEchoTool(), countingTool{runs: new(int)})
	history := []Entry{{Kind: EntryUser, User: &UserTurn{Text: "task"}}}
	scope := RequestScope{SessionID: "s", RunID: "r", Step: 1}

	req := BuildContext(cfg, scope, history)
	again := BuildContext(cfg, scope, history)

	if req.Instructions != again.Instructions || !strings.Contains(req.Instructions, "re:agent") {
		t.Fatal("instructions are not a stable embedded value")
	}
	if len(req.Tools) != 2 || req.Tools[0].Name != "counter" || req.Tools[1].Name != "echo" {
		t.Fatalf("tools are not sorted by name: %+v", req.Tools)
	}
	if req.Scope != scope || req.Model != cfg.Model {
		t.Fatalf("got %+v", req)
	}
}

// v1 §5.2: a later append to the run's history must not reach a request that
// was already built.
func TestBuildContext_HistoryIsCopied(t *testing.T) {
	cfg := testConfig(t)
	history := make([]Entry, 1, 4)
	history[0] = Entry{Kind: EntryUser, User: &UserTurn{Text: "task"}}

	req := BuildContext(cfg, RequestScope{Step: 1}, history)
	history = append(history, Entry{Kind: EntryAssistant, Assistant: &ModelResponse{}})

	if len(req.History) != 1 {
		t.Fatalf("built request grew to %d entries", len(req.History))
	}
}
