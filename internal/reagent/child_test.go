package reagent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

type childTestModel struct {
	generate func(context.Context, ModelRequest) (ModelResponse, error)
}

func (m childTestModel) Name() string { return "test" }
func (m childTestModel) Generate(ctx context.Context, req ModelRequest) (ModelResponse, error) {
	return m.generate(ctx, req)
}

func childFixture(t *testing.T, provider string, responses ...ModelResponse) (*Session, string) {
	t.Helper()
	t.Setenv("HOME", t.TempDir())
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "source.go"), []byte("approved evidence\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "secret.go"), []byte("UNSHARED_CANARY_48"), 0o600); err != nil {
		t.Fatal(err)
	}
	ws, err := OpenWorkspace(root)
	if err != nil {
		t.Fatal(err)
	}
	registry, err := NewRegistry(Mode{}, NewChildRunTool(), NewReadFileTool(ws), NewWriteFileTool(ws))
	if err != nil {
		t.Fatal(err)
	}
	cfg := Config{Provider: provider, Model: "gpt-6-sol", Registry: registry, Workspace: ws, WorkspacePath: root, ChildRuns: true, MaxSteps: 15, MaxToolCalls: 20}
	s := NewSession(cfg, NewScriptedModel(responses...), NewTrace(io.Discard), io.Discard)
	sum := sha256.Sum256([]byte("approved evidence\n"))
	return s, hex.EncodeToString(sum[:])
}

func childArgs(kind, digest string) string {
	return `{"kind":"` + kind + `","task":"inspect approved evidence","specification":"preserve public behavior","files":[{"path":"source.go","sha256":"` + digest + `"}]}`
}

func childOutcome(t *testing.T, s *Session, idx int) struct {
	Status      RunStatus `json:"status"`
	ReportValid bool      `json:"report_valid"`
	SnapshotID  string    `json:"snapshot_id"`
	SessionID   string    `json:"session_id"`
	RunID       string    `json:"run_id"`
	TracePath   string    `json:"trace_path"`
	Steps       int       `json:"steps"`
	Attempts    int       `json:"attempts"`
} {
	t.Helper()
	var data struct {
		Status      RunStatus `json:"status"`
		ReportValid bool      `json:"report_valid"`
		SnapshotID  string    `json:"snapshot_id"`
		SessionID   string    `json:"session_id"`
		RunID       string    `json:"run_id"`
		TracePath   string    `json:"trace_path"`
		Steps       int       `json:"steps"`
		Attempts    int       `json:"attempts"`
	}
	if err := json.Unmarshal(results(s)[idx].Outcome.Data, &data); err != nil {
		t.Fatal(err)
	}
	return data
}

func TestChild_TwoFreshReviewersInspectEncodedFirstRequests(t *testing.T) {
	for _, provider := range []string{openaiName, anthropicName} {
		t.Run(provider, func(t *testing.T) {
			s, digest := childFixture(t, provider)
			first := childArgs("review", digest)
			second := `{"kind":"review","task":"inspect approved evidence","specification":"preserve public behavior","snapshot_id":"` // filled by parent model below
			canary := "PARENT_TRANSCRIPT_CANARY_48"
			s.history = []Entry{
				{Kind: EntryUser, User: &UserTurn{Text: canary}},
				{Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{textBlock("old")}, Native: NativeOutput{Provider: provider, Items: []json.RawMessage{json.RawMessage(`{"secret":"NATIVE_CANARY_48"}`)}}}},
				{Kind: EntrySummary, Summary: &Summary{Text: "HANDOFF_CANARY_48"}},
			}
			var requests [][]byte
			s.childModel = func(_ string, _ *Trace) Model {
				return childTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
					body, err := encodeRequest(s.cfg, req)
					if err != nil {
						t.Fatal(err)
					}
					requests = append(requests, body)
					return turn(textBlock(`{"summary":"evidence reviewed","findings":[{"severity":"info","location":"source.go","description":"SIBLING_FINDING_CANARY_48"}],"verdict":"concerns"}`)), nil
				}}
			}
			count := 0
			s.model = childTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
				count++
				switch count {
				case 1:
					return turn(callBlock("c1", "child_run", first)), nil
				case 2:
					id := childOutcome(t, s, 0).SnapshotID
					return turn(callBlock("c2", "child_run", second+id+`"}`)), nil
				default:
					return turn(textBlock("done")), nil
				}
			}}
			result, err := s.Turn(context.Background(), "AUTHOR_RATIONALE_CANARY_48", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
			if err != nil || result.Status != StatusCompleted {
				t.Fatalf("turn: %+v %v", result, err)
			}
			if result.Children != 2 || result.ChildSteps != 2 || result.ChildUsage == nil {
				t.Fatalf("accounting: %+v", result)
			}
			if len(requests) != 2 {
				t.Fatalf("child requests: %d", len(requests))
			}
			for i, body := range requests {
				for _, unwanted := range []string{canary, "HANDOFF_CANARY_48", "AUTHOR_RATIONALE_CANARY_48", "UNSHARED_CANARY_48", "NATIVE_CANARY_48", "SIBLING_FINDING_CANARY_48"} {
					if strings.Contains(string(body), unwanted) {
						t.Fatalf("request %d contaminated by %s", i, unwanted)
					}
				}
				if !strings.Contains(string(body), "preserve public behavior") || !strings.Contains(string(body), "source.go") || strings.Contains(string(body), "child_run") || strings.Contains(string(body), "write_file") {
					t.Fatalf("wrong child boundary: %s", body)
				}
			}
			one, two := childOutcome(t, s, 0), childOutcome(t, s, 1)
			if !one.ReportValid || !two.ReportValid || one.SnapshotID != two.SnapshotID || one.SessionID == two.SessionID || one.RunID == two.RunID || one.TracePath == two.TracePath {
				t.Fatalf("reviews not independent: %+v %+v", one, two)
			}
		})
	}
}

func TestChild_ResearchMalformedAndDenied(t *testing.T) {
	s, digest := childFixture(t, openaiName)
	s.model = NewScriptedModel(
		turn(callBlock("c1", "child_run", childArgs("research", digest))),
		turn(callBlock("c2", "child_run", strings.Replace(childArgs("review", digest), digest, strings.Repeat("0", 64), 1))),
		turn(textBlock("done")))
	s.childModel = func(_ string, _ *Trace) Model {
		return NewScriptedModel(turn(textBlock(`{"summary":"research","findings":[],"verdict":"inconclusive"}`)))
	}
	result, err := s.Turn(context.Background(), "task", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
	if err != nil || result.Status != StatusCompleted || !childOutcome(t, s, 0).ReportValid || results(s)[1].Outcome.Code != "snapshot_denied" {
		t.Fatalf("result %+v observations %+v: %v", result, results(s), err)
	}
	for _, report := range []string{"", "not JSON", `{"summary":"okay","verdict":"no_findings"}`, `{"summary":"okay","findings":[],"verdict":"no_findings","extra":true}`, `{"summary":"okay","findings":[],"verdict":"no_findings"} {}`} {
		if _, err := childReport(report); err == nil {
			t.Errorf("accepted invalid report %q", report)
		}
	}
}

func TestChild_ManifestDenials(t *testing.T) {
	for _, tc := range []struct{ name, mutate, code string }{
		{"missing", "missing.go", "snapshot_denied"},
		{"withheld", ".env", "snapshot_denied"},
		{"traversal", "../source.go", "snapshot_denied"},
		{"unknown nested field", `source.go","extra":true`, "invalid_arguments"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, digest := childFixture(t, openaiName)
			args := strings.Replace(childArgs("review", digest), "source.go", tc.mutate, 1)
			s.model = NewScriptedModel(turn(callBlock("child", "child_run", args)), turn(textBlock("done")))
			s.childModel = func(_ string, _ *Trace) Model { t.Fatal("denied manifest launched child"); return nil }
			result, err := s.Turn(context.Background(), "task", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
			if err != nil || result.Status != StatusCompleted || results(s)[0].Outcome.Code != tc.code {
				t.Fatalf("denial: %+v %+v %v", result, results(s), err)
			}
		})
	}
}

func TestChild_PlanAndReadOnlyCeiling(t *testing.T) {
	s, digest := childFixture(t, openaiName)
	registry, err := NewRegistry(Mode{ReadOnly: true}, NewChildRunTool(), NewReadFileTool(s.cfg.Workspace), NewWriteFileTool(s.cfg.Workspace))
	if err != nil {
		t.Fatal(err)
	}
	s.cfg.Registry = registry.bindWorkspace(s, s.workspace.active)
	s.cfg.PlanMode, s.planMode = true, true
	s.model = NewScriptedModel(turn(callBlock("child", "child_run", childArgs("review", digest))), turn(textBlock("plan")))
	s.childModel = func(_ string, _ *Trace) Model {
		return childTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			if !strings.Contains(req.Instructions, "Mode: read only") || len(req.History) != 1 || req.History[0].User.Plan != "on" {
				t.Fatalf("child gained authority: %+v", req)
			}
			for _, spec := range req.Tools {
				if spec.Effect != EffectClassRead || spec.Name == "child_run" {
					t.Fatalf("child tool %s leaked", spec.Name)
				}
			}
			return turn(textBlock(`{"summary":"plan","findings":[],"verdict":"inconclusive"}`)), nil
		}}
	}
	result, err := s.Turn(context.Background(), "review", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
	if err != nil || result.Status != StatusCompleted || !childOutcome(t, s, 0).ReportValid {
		t.Fatalf("plan child: %+v %v", result, err)
	}
}

func TestChild_AttemptAdmissionIncludesRetries(t *testing.T) {
	var requests int
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { requests++; w.WriteHeader(http.StatusServiceUnavailable) }))
	defer server.Close()
	c := &childState{attempts: maxChildAttempts - 1}
	tr := NewTrace(io.Discard)
	transport := &transport{endpoint: server.URL, client: server.Client(), trace: tr}
	ctx := context.WithValue(context.Background(), childAttemptKey{}, c)
	_, _, err := transport.call(ctx, 1, []byte(`{}`), nil)
	var modelErr *ModelError
	if !errors.As(err, &modelErr) || modelErr.Status != StatusLimitExceeded || requests != 1 || c.attempts != maxChildAttempts {
		t.Fatalf("attempts=%d requests=%d err=%v", c.attempts, requests, err)
	}
}

func TestChild_RefusalMalformedFailureAndReadOnly(t *testing.T) {
	cases := []struct {
		name     string
		response ModelResponse
		status   RunStatus
	}{
		{"refusal", turn(refusalBlock("no")), StatusRefused},
		{"malformed report", turn(textBlock(`{"summary":"okay","verdict":"no_findings"}`)), StatusCompleted},
		{"protocol failure", turn(), StatusProtocolError},
		{"write denied", turn(callBlock("write", "write_file", `{"path":"source.go","content":"changed"}`)), StatusCompleted},
		{"recursion denied", turn(callBlock("nested", "child_run", `{}`)), StatusCompleted},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			s, digest := childFixture(t, openaiName)
			s.model = NewScriptedModel(turn(callBlock("child", "child_run", childArgs("review", digest))), turn(textBlock("done")))
			if tc.name == "write denied" || tc.name == "recursion denied" {
				s.childModel = func(_ string, _ *Trace) Model {
					return NewScriptedModel(tc.response, turn(textBlock(`{"summary":"could not write","findings":[],"verdict":"inconclusive"}`)))
				}
			} else {
				s.childModel = func(_ string, _ *Trace) Model { return NewScriptedModel(tc.response) }
			}
			result, err := s.Turn(context.Background(), "task", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
			if err != nil || result.Status != StatusCompleted {
				t.Fatalf("parent: %+v %v", result, err)
			}
			data := childOutcome(t, s, 0)
			if tc.name == "write denied" || tc.name == "recursion denied" {
				if !data.ReportValid || data.Status != StatusCompleted {
					t.Fatalf("denial: %+v", data)
				}
				var denied bool
				for _, event := range readEvents(t, data.TracePath) {
					if event.Type != "tool.finished" {
						continue
					}
					tool := event.Data.(map[string]any)
					denied = tool["outcome"].(map[string]any)["code"] == "tool_unavailable"
				}
				if !denied {
					t.Fatal("child attempted to execute an undeclared capability")
				}
				bytes, err := os.ReadFile(filepath.Join(s.cfg.WorkspacePath, "source.go"))
				if err != nil || string(bytes) != "approved evidence\n" {
					t.Fatalf("parent workspace changed: %q %v", bytes, err)
				}
			} else if data.ReportValid || data.Status != tc.status {
				t.Fatalf("child: %+v", data)
			}
		})
	}
}

func TestChild_FourChildrenBoundAndFrozenSnapshot(t *testing.T) {
	s, digest := childFixture(t, openaiName)
	var requests []string
	childNumber := 0
	s.childModel = func(_ string, _ *Trace) Model {
		childNumber++
		number, step := childNumber, 0
		return childTestModel{generate: func(_ context.Context, req ModelRequest) (ModelResponse, error) {
			step++
			body, err := encodeRequest(s.cfg, req)
			if err != nil {
				t.Fatal(err)
			}
			if step == 1 {
				requests = append(requests, string(body))
			}
			if number == 2 && step == 1 {
				response := turn(callBlock("read", "read_file", `{"path":"source.go"}`))
				response.Native = NativeOutput{Provider: openaiProvider, Items: []json.RawMessage{json.RawMessage(`{"type":"function_call","call_id":"read","name":"read_file","arguments":"{\"path\":\"source.go\"}"}`)}}
				return response, nil
			}
			if number == 2 && (!strings.Contains(string(body), "approved evidence") || strings.Contains(string(body), "changed after capture")) {
				t.Fatalf("frozen file bytes changed in child request: %s", body)
			}
			return turn(textBlock(`{"summary":"reviewed","findings":[],"verdict":"no_findings"}`)), nil
		}}
	}
	calls := 0
	s.model = childTestModel{generate: func(_ context.Context, _ ModelRequest) (ModelResponse, error) {
		calls++
		if calls == 1 {
			return turn(callBlock("child-1", "child_run", childArgs("review", digest))), nil
		}
		if calls <= 5 {
			id := childOutcome(t, s, 0).SnapshotID
			if calls == 2 {
				if err := os.WriteFile(filepath.Join(s.cfg.WorkspacePath, "source.go"), []byte("changed after capture\n"), 0o600); err != nil {
					t.Fatal(err)
				}
			}
			return turn(callBlock("child-"+string(rune('0'+calls)), "child_run", `{"kind":"review","task":"inspect","specification":"preserve","snapshot_id":"`+id+`"}`)), nil
		}
		return turn(textBlock("done")), nil
	}}
	result, err := s.Turn(context.Background(), "task", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
	if err != nil || result.Status != StatusCompleted || result.Children != 4 || result.ChildSteps != 5 || len(requests) != 4 {
		t.Fatalf("result: %+v requests=%d err=%v", result, len(requests), err)
	}
	if results(s)[4].Outcome.Code != "limit_exceeded" {
		t.Fatalf("fifth child was not denied: %+v", results(s)[4])
	}
	for _, req := range requests {
		if !strings.Contains(req, digest) {
			t.Fatal("reused snapshot changed after source mutation")
		}
	}
}

func TestChild_ProviderFailureIsNotApproval(t *testing.T) {
	s, digest := childFixture(t, openaiName)
	s.model = NewScriptedModel(turn(callBlock("child", "child_run", childArgs("review", digest))), turn(textBlock("done")))
	s.childModel = func(_ string, _ *Trace) Model {
		return childTestModel{generate: func(_ context.Context, _ ModelRequest) (ModelResponse, error) {
			return ModelResponse{}, &ModelError{Status: StatusProviderError, Message: "offline failure", Usage: Usage{Known: true, InputTokens: 17}}
		}}
	}
	result, err := s.Turn(context.Background(), "task", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
	if err != nil || result.Status != StatusCompleted || childOutcome(t, s, 0).ReportValid || result.ChildUsage == nil || result.ChildUsage.InputTokens != 17 {
		t.Fatalf("failure: %+v %+v %v", result, childOutcome(t, s, 0), err)
	}
}

func TestChild_CancellationAndExhaustion(t *testing.T) {
	s, digest := childFixture(t, openaiName)
	s.model = NewScriptedModel(turn(callBlock("c1", "child_run", childArgs("review", digest))))
	ctx, cancel := context.WithCancel(context.Background())
	s.childModel = func(_ string, _ *Trace) Model {
		return childTestModel{generate: func(ctx context.Context, _ ModelRequest) (ModelResponse, error) {
			cancel()
			return ModelResponse{}, &ModelError{Status: StatusCancelled, Message: "cancelled"}
		}}
	}
	result, err := s.Turn(ctx, "task", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
	if err != nil || result.Status != StatusCancelled || childOutcome(t, s, 0).ReportValid {
		t.Fatalf("cancellation %+v %v", result, err)
	}
	path := childOutcome(t, s, 0).TracePath
	if path == "" {
		t.Fatal("missing child trace")
	}
	started := readEvents(t, path)[0].Data.(map[string]any)
	if _, err := os.Stat(started["workspace"].(string)); !os.IsNotExist(err) {
		t.Fatalf("temporary child workspace was not removed: %v", err)
	}
	if reason := (&childState{children: maxChildren}).exhausted(); reason == "" {
		t.Fatal("child count not bounded")
	}
	if reason := (&childState{steps: maxChildSteps}).exhausted(); reason == "" {
		t.Fatal("child steps not bounded")
	}
	if reason := (&childState{calls: maxChildCalls}).exhausted(); reason == "" {
		t.Fatal("child calls not bounded")
	}
	if reason := (&childState{attempts: maxChildAttempts}).exhausted(); reason == "" {
		t.Fatal("child attempts not bounded")
	}
	if _, err := childReport(`{"summary":"okay","findings":[],"verdict":"inconclusive"}`); err != nil {
		t.Fatal(err)
	}
	if !errors.Is(ctx.Err(), context.Canceled) {
		t.Fatal("parent cancellation was lost")
	}
}

func TestChild_ResumeRetainsOptInButDoesNotRestartInterruptedChild(t *testing.T) {
	s, _ := childFixture(t, openaiName)
	s.store = testSessionStore(t, s.ID)
	s.history = []Entry{
		{Kind: EntryUser, User: &UserTurn{Text: "review"}},
		{Kind: EntryAssistant, Assistant: &ModelResponse{Blocks: []OutputBlock{callBlock("child", "child_run", `{}`)}}},
	}
	s.seenCalls["child"] = true
	if err := s.checkpoint("tool", "child"); err != nil {
		t.Fatal(err)
	}
	cp, err := s.store.load(s.ID)
	if err != nil || !cp.ChildRuns {
		t.Fatalf("checkpoint: %+v %v", cp, err)
	}
	resumed := NewSession(s.cfg, NewScriptedModel(turn(textBlock("new input"))), NewTrace(io.Discard), io.Discard)
	resumed.restore(cp)
	resumed.store = s.store
	if !resumed.cfg.ChildRuns {
		t.Fatal("lost child opt-in on restore")
	}
	message, err := resumed.recoverInterrupted()
	if err != nil || !strings.Contains(message, "unknown effects") || len(results(resumed)) != 1 || results(resumed)[0].Outcome.Code != "recovery_effect_unknown" {
		t.Fatalf("recovery: %q %+v %v", message, results(resumed), err)
	}
}

func TestChild_OptInPreviewOnlyDeclaresChildWhenEnabled(t *testing.T) {
	root := t.TempDir()
	for _, tc := range []struct {
		enabled bool
		flags   []string
	}{
		{false, nil}, {true, []string{"--child-runs"}},
	} {
		var out, stderr bytes.Buffer
		args := []string{"run", "--workspace", root, "--provider", "openai", "--model", "gpt-6-sol", "--show-context"}
		args = append(args, tc.flags...)
		args = append(args, "review evidence")
		if code := Main(context.Background(), args, strings.NewReader(""), &out, &stderr); code != 0 {
			t.Fatalf("preview: %d %s", code, stderr.String())
		}
		if got := strings.Contains(out.String(), `"name":"child_run"`); got != tc.enabled {
			t.Fatalf("child tool declaration enabled=%v present=%v", tc.enabled, got)
		}
	}
}

func TestChild_FreshModelSwitchRetainsAdapterBinding(t *testing.T) {
	s, digest := childFixture(t, openaiName)
	s.childModel = func(_ string, _ *Trace) Model {
		return NewScriptedModel(turn(textBlock(`{"summary":"fresh review","findings":[],"verdict":"no_findings"}`)))
	}
	c := &conversation{session: s, trace: s.trace, progress: io.Discard, client: NewHTTPClient(), keys: map[string]string{openaiName: "test"}}
	info, _ := findModel("gpt-6-sol")
	c.switchTo(info)
	if c.session.childModel == nil || !c.session.cfg.ChildRuns {
		t.Fatal("fresh model switch discarded child capability")
	}
	c.session.model = NewScriptedModel(turn(callBlock("child", "child_run", childArgs("review", digest))), turn(textBlock("done")))
	result, err := c.session.Turn(context.Background(), "review", NewID(), filepath.Join(t.TempDir(), "parent.jsonl"))
	if err != nil || result.Status != StatusCompleted || !childOutcome(t, c.session, 0).ReportValid {
		t.Fatalf("fresh switch child: %+v %v", result, err)
	}
}
