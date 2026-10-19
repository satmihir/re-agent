package reagent

import (
	"bytes"
	"context"
	"io"
	"strings"
	"sync"
	"testing"
)

type statusModel struct {
	replies []ModelResponse
	observe func(int)
	calls   int
}

func (*statusModel) Name() string { return "status" }
func (m *statusModel) Generate(_ context.Context, _ ModelRequest) (ModelResponse, error) {
	m.observe(m.calls)
	reply := m.replies[m.calls]
	m.calls++
	return reply, nil
}

func TestChat_StatusRefreshesWithinTurn(t *testing.T) {
	var out bytes.Buffer
	input := terminalInput(strings.NewReader(""))
	input.region = &terminalRegion{out: &out, mu: &sync.Mutex{}, width: 100, height: 24}
	cfg := testConfig(t)
	cfg.ContextWindowTokens = 100_000
	model := &statusModel{replies: []ModelResponse{
		turn(callBlock("first", "echo", `{"text":"first"}`)),
		turn(callBlock("second", "echo", `{"text":"second"}`)),
		turn(textBlock("done")),
	}}
	model.replies[0].Usage = Usage{Known: true, InputTokens: 12_000}
	model.replies[1].Usage = Usage{Known: true, InputTokens: 42_000}
	s := NewSession(cfg, model, NewTrace(io.Discard), &out)
	s.lastRequest = Usage{Known: true, InputTokens: 4_000}
	s.display.attachRegion(input)
	c := &conversation{session: s, traceDir: t.TempDir(), progress: &out, usage: Usage{Known: true}}
	model.observe = func(call int) {
		input.region.mu.Lock()
		defer input.region.mu.Unlock()
		want := []int64{4_000, 12_000, 42_000}[call]
		if !input.regionStatus.usage.Known || input.regionStatus.usage.InputTokens != want {
			t.Errorf("request %d status usage = %+v, want %d input tokens", call+1, input.regionStatus.usage, want)
		}
		status := c.regionStatus()
		if input.regionStatus.model != status.model || input.regionStatus.effort != status.effort {
			t.Errorf("status route = %+v, want %+v", input.regionStatus, status)
		}
	}
	c.runTurn(context.Background(), "check status", input, &out, &out)
	if model.calls != 3 {
		t.Fatalf("model calls = %d, want 3", model.calls)
	}
}
