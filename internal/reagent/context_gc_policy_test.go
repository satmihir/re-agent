package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"math"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// Keep a substantial unchanged history prefix before the obsolete outcome:
// charging only the cache-break suffix would incorrectly make the late pass cheap.
func gcEconomicFixture(t *testing.T, provider string, requests int) (*Session, *Run) {
	t.Helper()
	s := gcFixture(t, provider)
	s.history = append(s.history, Entry{Kind: EntryUser, User: &UserTurn{Text: strings.Repeat("prefix", 9000)}})
	gcAppend(s, "old", "exec", `{"argv":["same"],"cwd":"."}`, ToolOutcome{OK: true, Message: strings.Repeat("old", 8000), Effect: EffectNone})
	gcAppend(s, "new", "exec", `{"argv":["same"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectNone})
	gcAge(s, 15)
	r := gcRun(s)
	s.gcRequests = requests
	size, err := admissionRequestBytes(r.cfg, BuildContext(r.cfg, RequestScope{}, s.history))
	if err != nil {
		t.Fatal(err)
	}
	s.tokensPerByte = float64(s.lastRequest.InputTokens) / float64(size)
	return s, r
}

func TestContextGC_WindowFloor(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		for _, window := range []int64{200_000, 1_050_000} {
			for _, delta := range []int64{-1, 0} {
				s, r := gcEconomicFixture(t, provider, 128)
				r.window = window
				last := s.lastRequest.InputTokens
				s.lastRequest.InputTokens = int64(float64(window)*contextGCWindowFloor) + delta
				s.tokensPerByte *= float64(s.lastRequest.InputTokens) / float64(last)
				if got := r.collectContext(); got != (delta == 0) {
					t.Fatalf("provider=%s window=%d delta=%d pass=%v", provider, window, delta, got)
				}
			}
		}
	}
	s, r := gcEconomicFixture(t, openaiName, 128)
	r.window = 0
	if r.collectContext() || len(s.gc.Dropped) != 0 {
		t.Fatal("unknown window must not pass")
	}
}

func TestContextGC_BreakEvenAndAnthropicWrites(t *testing.T) {
	for _, test := range []struct {
		provider string
		requests int
		want     bool
	}{
		{openaiName, 16, false},
		{openaiName, 32, true},
		{anthropicName, 16, false},
		{anthropicName, 32, true},
		// A 25% cache-write premium changes the decision, not only reporting.
		{openaiName, 24, true},
		{anthropicName, 24, false},
	} {
		s, r := gcEconomicFixture(t, test.provider, test.requests)
		if got := r.collectContext(); got != test.want {
			t.Fatalf("provider=%s requests=%d pass=%v want=%v", test.provider, test.requests, got, test.want)
		}
		if !test.want && len(s.gc.Dropped) != 0 {
			t.Fatal("blocked pass changed the view")
		}
	}
}

func TestContextGC_NearCompactionException(t *testing.T) {
	for _, test := range []struct {
		name        string
		near        bool
		enabled     bool
		blocked     bool
		compactions int
		want        bool
	}{
		{"near", true, true, false, 0, true},
		{"far", false, true, false, 0, false},
		{"off", true, false, false, 0, false},
		{"compact disabled after failure", true, true, true, 0, false},
		{"below summary cap", true, true, false, inRunCompactLimit - 1, true},
		{"at summary cap", true, true, false, inRunCompactLimit, false},
		{"above summary cap", true, true, false, inRunCompactLimit + 1, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			s, r := gcEconomicFixture(t, openaiName, 16)
			r.cfg.InRunCompact = test.enabled
			r.compactOff = test.blocked
			r.compactions = test.compactions
			r.steps = 17
			size, err := admissionRequestBytes(r.cfg, BuildContext(r.cfg, RequestScope{}, s.history))
			if err != nil {
				t.Fatal(err)
			}
			before := math.Ceil(float64(size) * s.admissionRate(r.cfg.Provider))
			share := 0.90
			if !test.near {
				share = 0.89
			}
			r.window = int64(math.Ceil(before / (0.4 * share)))
			if test.near {
				r.window--
			}
			if got := r.collectContext(); got != test.want {
				t.Fatalf("pass=%v want=%v", got, test.want)
			}
			if test.want {
				r.model = NewScriptedModel()
				if status, reason := r.compactWithinRunAfterGC(context.Background(), true); status != "" || reason != "" || r.steps != 17 {
					t.Fatalf("GC failed to avoid next compaction: %s %s", status, reason)
				}
			}
		})
	}
	// Close alone is insufficient if the projected view still crosses the trigger.
	s, r := gcEconomicFixture(t, openaiName, 16)
	r.cfg.InRunCompact, r.steps = true, 17
	r.window = 40_000
	if r.collectContext() || len(s.gc.Dropped) != 0 {
		t.Fatal("near pass must put the next request below the compaction trigger")
	}
}

func TestContextGC_CostTraceAndRemainingBounds(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		for _, requests := range []int{0, 16, 32, 48, 1000} {
			s := gcFixture(t, provider)
			gcAppend(s, "old", "exec", `{"argv":["same"],"cwd":"."}`, ToolOutcome{OK: true, Message: strings.Repeat("old", 12000), Effect: EffectNone})
			gcAppend(s, "new", "exec", `{"argv":["same"],"cwd":"."}`, ToolOutcome{OK: true, Effect: EffectNone})
			gcAge(s, 15)
			r := gcRun(s)
			s.gcRequests = requests
			path := filepath.Join(t.TempDir(), "events.jsonl")
			trace := OpenTrace(path, s.ID, "gc", io.Discard)
			r.trace = trace
			if !r.collectContext() {
				t.Fatal("no pass")
			}
			trace.Close()
			data, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			var event struct {
				Data map[string]json.RawMessage `json:"data"`
			}
			if err := json.Unmarshal(bytes.TrimSpace(data), &event); err != nil {
				t.Fatal(err)
			}
			var got struct {
				Fixed     int     `json:"fixed_prefix_bytes"`
				Rebuild   int     `json:"rebuild_bytes"`
				Tokens    int64   `json:"estimated_rebuild_tokens"`
				Remaining int     `json:"expected_remaining_requests"`
				Requests  int     `json:"session_requests"`
				Write     float64 `json:"cache_write_price"`
				Ratio     float64 `json:"cached_ratio"`
				Trigger   string  `json:"trigger"`
				Net       float64 `json:"estimated_net_saving"`
				Break     int     `json:"cache_break_byte"`
			}
			fields, _ := json.Marshal(event.Data)
			if err := json.Unmarshal(fields, &got); err != nil {
				t.Fatal(err)
			}
			req := BuildContext(r.cfg, RequestScope{}, s.requestHistory())
			body, err := encodeRequest(r.cfg, req)
			if err != nil {
				t.Fatal(err)
			}
			var wire map[string]json.RawMessage
			if err := json.Unmarshal(body, &wire); err != nil {
				t.Fatal(err)
			}
			key, write := "instructions", 1.0
			if provider == anthropicName {
				key, write = "system", 1.25
			}
			fixed := len(wire[key]) + len(wire["tools"])
			size, err := admissionRequestBytes(r.cfg, req)
			if err != nil {
				t.Fatal(err)
			}
			if got.Fixed != fixed || got.Rebuild != size-fixed || got.Tokens != int64(math.Ceil(float64(size-fixed)*s.tokensPerByte)) ||
				got.Remaining != max(16, min(48, requests)) || got.Requests != requests || got.Write != write || got.Ratio != .1 ||
				got.Trigger != "break_even" || got.Net <= 0 || got.Break <= 0 {
				t.Fatalf("trace: %+v fixed=%d size=%d", got, fixed, size)
			}
		}
	}
}

func TestContextGC_SessionRequestsSurviveResumeAndReset(t *testing.T) {
	s := gcFixture(t, openaiName)
	s.gcRequests = 127
	s.model = NewScriptedModel(contextResponse(openaiName, textBlock("done")))
	s.history = nil
	s.store = &sessionStore{dir: t.TempDir()}
	if result, err := s.Turn(context.Background(), "next", "counter", filepath.Join(t.TempDir(), "events.jsonl")); err != nil || result.Status != StatusCompleted {
		t.Fatalf("turn: %+v %v", result, err)
	}
	if s.gcRequests != 128 {
		t.Fatalf("requests=%d", s.gcRequests)
	}
	cp, err := s.store.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	restored := gcFixture(t, openaiName)
	restored.restore(cp)
	if restored.gcRequests != 128 {
		t.Fatal("counter lost on resume")
	}
	cp.GCRequests = 0
	restored.restore(cp)
	if restored.gcRequests != 1 {
		t.Fatalf("legacy recovery=%d", restored.gcRequests)
	}
	s.model = NewScriptedModel(contextResponse(openaiName, textBlock("summary")))
	if result, _, err := s.Compact(context.Background(), "", "summary", filepath.Join(t.TempDir(), "events.jsonl")); err != nil || result.Status != StatusCompleted {
		t.Fatalf("compact: %+v %v", result, err)
	}
	if s.gcRequests != 128 {
		t.Fatal("summary changed Session request estimate")
	}
	cp, err = s.store.load(s.ID)
	if err != nil {
		t.Fatal(err)
	}
	restored.restore(cp)
	if restored.gcRequests != 128 {
		t.Fatal("counter lost after compaction and resume")
	}
	s.Reset()
	if s.gcRequests != 0 {
		t.Fatal("fresh Session retained counter")
	}
}
