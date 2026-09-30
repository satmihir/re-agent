package reagent

import (
	"context"
	"encoding/json"
	"strings"
	"sync"
	"unicode/utf8"
)

type reportFrictionTool struct {
	mu      sync.Mutex
	reports int
}

// NewReportFrictionTool returns a trace-only reporter capped for the process lifetime.
func NewReportFrictionTool() Tool { return &reportFrictionTool{} }

type reportFrictionArgs struct {
	Category       string          `json:"category"`
	Summary        string          `json:"summary"`
	Details        json.RawMessage `json:"details"`
	RelatedCallIDs json.RawMessage `json:"related_call_ids"`
}

func (*reportFrictionTool) Spec() ToolSpec {
	return ToolSpec{
		Name: "report_friction",
		Description: "Report a rough edge in the re:agent harness itself: a misleading tool error, " +
			"a missing capability you worked around, an unclear description or instruction, or a harness bug. " +
			"Reports are for re:agent's developers; they do not change anything in this session. Use it briefly and continue your task.",
		InputSchema: json.RawMessage(`{
  "type": "object",
  "properties": {
    "category": {"type": "string", "enum": ["misleading_error", "missing_capability", "unclear_description", "harness_bug", "other"], "description": "Kind of rough edge in the harness."},
    "summary": {"type": "string", "minLength": 1, "maxLength": 300, "description": "One nonblank line describing what happened, at most 300 Unicode characters."},
    "details": {"type": "string", "maxLength": 4000, "description": "Optional explanation, at most 4000 Unicode characters."},
    "related_call_ids": {"type": "array", "items": {"type": "string"}, "maxItems": 20, "description": "Optional IDs of up to 20 calls involved; their existence is not checked."}
  },
  "required": ["category", "summary"],
  "additionalProperties": false
}`),
		Effect: EffectClassRead,
	}
}

func (t *reportFrictionTool) Execute(_ context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a reportFrictionArgs
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	switch a.Category {
	case "misleading_error", "missing_capability", "unclear_description", "harness_bug", "other":
	default:
		return failOutcome("invalid_arguments", "category must be misleading_error, missing_capability, unclear_description, harness_bug, or other"), nil
	}
	if strings.TrimSpace(a.Summary) == "" || strings.ContainsAny(a.Summary, "\r\n") || utf8.RuneCountInString(a.Summary) > 300 {
		return failOutcome("invalid_arguments", "summary must be one nonblank line of at most 300 Unicode characters"), nil
	}
	if len(a.Details) != 0 {
		var details string
		if string(a.Details) == "null" || json.Unmarshal(a.Details, &details) != nil || utf8.RuneCountInString(details) > 4000 {
			return failOutcome("invalid_arguments", "details must be a string of at most 4000 Unicode characters, or omitted"), nil
		}
	}
	if len(a.RelatedCallIDs) != 0 {
		var ids []json.RawMessage
		if string(a.RelatedCallIDs) == "null" || json.Unmarshal(a.RelatedCallIDs, &ids) != nil || len(ids) > 20 {
			return failOutcome("invalid_arguments", "related_call_ids must be an array of at most 20 strings, or omitted"), nil
		}
		for _, raw := range ids {
			var id string
			if string(raw) == "null" || json.Unmarshal(raw, &id) != nil {
				return failOutcome("invalid_arguments", "related_call_ids must contain only strings"), nil
			}
		}
	}
	// v0 §10 amendment (2026-09-30): invalid submissions do not consume the process cap.
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.reports >= 10 {
		return failOutcome("invalid_arguments", "report limit reached; carry on with the task"), nil
	}
	t.reports++
	return okOutcome(struct {
		Recorded bool `json:"recorded"`
	}{Recorded: true})
}
