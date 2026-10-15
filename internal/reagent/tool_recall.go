package reagent

import (
	"context"
	"encoding/json"
	"fmt"
)

type recallTool struct{ session *Session }

func (recallTool) Spec() ToolSpec {
	return ToolSpec{Name: "recall", Description: "Retrieve an original tool outcome hidden by context GC using its recall call ID. Returns the recorded bounded outcome verbatim as data, with a changed-since note and current digest for stale file reads. IDs not dropped or removed by compaction return not_found. Recall does not authorize a write.", InputSchema: json.RawMessage(`{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}`), Effect: EffectClassRead}
}

func (t recallTool) Execute(_ context.Context, args json.RawMessage) (ToolOutcome, error) {
	var a struct {
		ID string `json:"id"`
	}
	if bad := decodeArgs(args, &a); bad != nil {
		return *bad, nil
	}
	if a.ID == "" {
		return failOutcome("invalid_arguments", "id must not be empty"), nil
	}
	s := t.session
	if s == nil {
		return failOutcome("not_found", "no context GC result with that ID"), nil
	}
	if _, ok := s.gc.Dropped[a.ID]; !ok {
		return failOutcome("not_found", "ID was not dropped by context GC"), nil
	}
	original, found := s.gc.Inherited[a.ID]
	for _, entry := range s.history {
		if entry.Tool != nil && entry.Tool.CallID == a.ID && entry.Tool.Outcome.Code != "context_gc" {
			original, found = entry.Tool.Outcome, true
			break
		}
	}
	if !found {
		return failOutcome("not_found", "original outcome was removed by compaction or reset"), nil
	}
	// Keep the envelope's effect none even when the recalled command wrote files.
	raw, err := json.Marshal(original)
	if err != nil {
		return ToolOutcome{}, fmt.Errorf("cannot encode recalled outcome: invalid JSON data")
	}
	out := ToolOutcome{OK: true, Code: "ok", Data: raw, Effect: EffectNone}
	var read readFileResult
	if json.Unmarshal(original.Data, &read) == nil && digestPattern.MatchString(read.SHA256) {
		now := s.gcDigest(s.gc.Paths[a.ID])
		if now != "" && now != read.SHA256 {
			out.Message = "File has changed since this read; current digest: " + now
		}
	}
	return out, nil
}
