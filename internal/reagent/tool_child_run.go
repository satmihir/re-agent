package reagent

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"
)

// v0 §3: one visible schema for generation and validation.
const childReportFormat = `Return a text-only JSON object with exactly these fields and no extras:
{"summary":"nonblank text","findings":[{"severity":"nonblank text","location":"file:line or other precise reference","description":"nonblank text"}],"verdict":"concerns"}
Use an empty findings array if none. Verdict must be concerns, no_findings, or inconclusive. Summary is at most 4096 bytes; findings at most 20; each severity at most 32 bytes, location 256 bytes, description 1024 bytes; the whole reply at most 16 KiB. Every field is required, including all three finding fields. Prefer only JSON, without commentary or a code fence. Completed is not verified success.`

type childRunTool struct{ session *Session }

// NewChildRunTool offers bounded fresh-context work when explicitly enabled.
func NewChildRunTool() Tool { return childRunTool{} }

func (childRunTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "child_run", Effect: EffectClassRead,
		Description: "Start one synchronous read-only fresh-context child for review or research. Supply a fixed manifest of workspace-relative UTF-8 files and exact digests, or reuse a returned snapshot_id. No parent conversation or sibling findings are sent. Returns a bounded report and terminal status; completed is not verified success. At most four children per parent run.",
		InputSchema: json.RawMessage(`{"type":"object","properties":{"kind":{"type":"string","enum":["review","research"]},"task":{"type":"string"},"specification":{"type":"string"},"files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"sha256":{"type":"string"}},"required":["path","sha256"],"additionalProperties":false}},"snapshot_id":{"type":"string"}},"required":["kind","task","specification"],"additionalProperties":false}`),
	}
}

type childRunArgs struct {
	Kind          string          `json:"kind"`
	Task          string          `json:"task"`
	Specification string          `json:"specification"`
	Files         json.RawMessage `json:"files"`
	SnapshotID    json.RawMessage `json:"snapshot_id"`
}

func (t childRunTool) Execute(ctx context.Context, raw json.RawMessage) (ToolOutcome, error) {
	var a childRunArgs
	if !json.Valid(raw) {
		return failOutcome("invalid_arguments", "arguments must be one JSON object"), nil
	}
	if bad := decodeArgs(raw, &a); bad != nil {
		return *bad, nil
	}
	if a.Kind != "review" && a.Kind != "research" || strings.TrimSpace(a.Task) == "" || strings.TrimSpace(a.Specification) == "" || len(a.Task) > 4096 || len(a.Specification) > 8192 {
		return failOutcome("invalid_arguments", "kind, task or specification is invalid or oversized"), nil
	}
	if (len(a.Files) == 0) == (len(a.SnapshotID) == 0) {
		return failOutcome("invalid_arguments", "supply files or snapshot_id, not both"), nil
	}
	s := t.session
	if s == nil || s.currentRun == nil || s.childModel == nil || s.workspace == nil {
		return failOutcome("permission_denied", "child runs are unavailable here"), nil
	}
	c := &s.currentRun.children
	if reason := c.exhausted(); reason != "" {
		return failOutcome("limit_exceeded", reason), nil
	}
	var snap childSnapshot
	if len(a.SnapshotID) != 0 {
		var id string
		if string(a.SnapshotID) == "null" || json.Unmarshal(a.SnapshotID, &id) != nil || id == "" {
			return failOutcome("invalid_arguments", "snapshot_id must be a string"), nil
		}
		var found bool
		snap, found = c.snapshots[id]
		if !found {
			return failOutcome("not_found", "snapshot_id does not belong to this run"), nil
		}
	} else {
		if string(a.Files) == "null" {
			return failOutcome("invalid_arguments", "files must be a nonempty array"), nil
		}
		var files []childFile
		dec := json.NewDecoder(strings.NewReader(string(a.Files)))
		dec.DisallowUnknownFields()
		if err := dec.Decode(&files); err != nil {
			return failOutcome("invalid_arguments", "files must be an array of path/digest objects without extra fields"), nil
		}
		for _, f := range files {
			if f.Path == "" || f.SHA256 == "" {
				return failOutcome("invalid_arguments", "each file needs path and sha256"), nil
			}
		}
		var err error
		snap, err = c.capture(ctx, s.workspace.active, files)
		if err != nil {
			return failOutcome("snapshot_denied", err.Error()), nil
		}
	}
	if c.start.IsZero() {
		c.start = time.Now()
		c.usage = Usage{Known: true}
	}
	c.children++ // Admission is charged even if the child fails or is cancelled.
	result, sessionID, runID, attempts, cleanup := c.execute(ctx, s, snap, a.Kind, a.Task, a.Specification)
	data := struct {
		Status      RunStatus       `json:"status"`
		Reason      string          `json:"reason,omitempty"`
		Report      json.RawMessage `json:"report,omitempty"`
		ReportValid bool            `json:"report_valid"`
		SnapshotID  string          `json:"snapshot_id"`
		SessionID   string          `json:"session_id,omitempty"`
		RunID       string          `json:"run_id,omitempty"`
		TracePath   string          `json:"trace_path,omitempty"`
		Steps       int             `json:"steps"`
		ToolCalls   int             `json:"tool_calls"`
		Attempts    int             `json:"attempts"`
		Usage       Usage           `json:"usage"`
	}{Status: result.Status, Reason: result.Reason, SnapshotID: snap.id, SessionID: sessionID, RunID: runID, TracePath: result.TracePath, Steps: result.Steps, ToolCalls: result.ToolCalls, Attempts: attempts, Usage: result.Usage}
	if result.Status == StatusCompleted && cleanup == nil {
		report, err := childReport(result.Reply)
		if err == nil {
			data.Report, data.ReportValid = report, true
		} else {
			data.Reason = err.Error()
		}
	}
	if cleanup != nil {
		data.Reason = fmt.Sprintf("child snapshot cleanup failed: %v", cleanup)
	}
	outcome, err := okOutcome(data)
	if err != nil {
		return outcome, err
	}
	if !data.ReportValid {
		outcome.OK, outcome.Code, outcome.Message = false, "child_not_approved", data.Reason
		if outcome.Message == "" {
			outcome.Message = string(data.Status)
		}
	}
	return outcome, nil
}

func childReport(reply string) (json.RawMessage, error) {
	if len(reply) > 16<<10 {
		return nil, fmt.Errorf("child report exceeds 16 KiB")
	}
	text := strings.TrimSpace(reply)
	if strings.HasPrefix(text, "```") {
		line, rest, ok := strings.Cut(text, "\n")
		if !ok || (line != "```" && line != "```json") || !strings.HasSuffix(rest, "\n```") {
			return nil, fmt.Errorf("child report has an invalid code fence")
		}
		text = strings.TrimSpace(strings.TrimSuffix(rest, "\n```"))
	} else if line, rest, ok := strings.Cut(text, "\n"); ok && len(line) <= 160 && (strings.HasSuffix(line, ":") || strings.HasSuffix(line, ".")) && strings.HasPrefix(strings.TrimSpace(rest), "{") {
		text = strings.TrimSpace(rest)
	}
	if !json.Valid([]byte(text)) {
		return nil, fmt.Errorf("child report is malformed JSON")
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal([]byte(text), &fields); err != nil || fields["summary"] == nil || fields["findings"] == nil || fields["verdict"] == nil || string(fields["findings"]) == "null" {
		return nil, fmt.Errorf("child report requires summary, findings and verdict")
	}
	var report struct {
		Summary  string `json:"summary"`
		Findings []struct {
			Severity    string `json:"severity"`
			Location    string `json:"location"`
			Description string `json:"description"`
		} `json:"findings"`
		Verdict string `json:"verdict"`
	}
	if bad := decodeArgs(json.RawMessage(text), &report); bad != nil {
		return nil, fmt.Errorf("invalid child report: %s", bad.Message)
	}
	if strings.TrimSpace(report.Summary) == "" || len(report.Summary) > 4096 || len(report.Findings) > 20 {
		return nil, fmt.Errorf("invalid child report summary or findings")
	}
	if report.Verdict != "concerns" && report.Verdict != "no_findings" && report.Verdict != "inconclusive" {
		return nil, fmt.Errorf("invalid child verdict")
	}
	for _, f := range report.Findings {
		if strings.TrimSpace(f.Severity) == "" || strings.TrimSpace(f.Location) == "" || strings.TrimSpace(f.Description) == "" || len(f.Severity) > 32 || len(f.Location) > 256 || len(f.Description) > 1024 {
			return nil, fmt.Errorf("invalid child finding")
		}
	}
	return json.Marshal(report)
}
