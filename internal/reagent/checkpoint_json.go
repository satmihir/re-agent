package reagent

import (
	"bytes"
	"encoding/json"
	"fmt"
)

// Checkpoints use declaration-order structs, sorted map keys, and no final LF.
func canonicalCheckpointJSON(v any) ([]byte, error) {
	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(v); err != nil {
		return nil, fmt.Errorf("checkpoint state cannot be encoded as JSON")
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

// Raw JSON is persisted as base64 strings, not embedded JSON: compaction would
// change native provider items. Copy every containing value before replacing it.
func checkpointRawJSON(cp chatCheckpoint, decode bool) (chatCheckpoint, error) {
	var conversionErr error
	raw := func(value json.RawMessage) json.RawMessage {
		if decode {
			var decoded []byte
			if len(value) != 0 && ((value[0] != '"' && !bytes.Equal(value, []byte("null"))) || json.Unmarshal(value, &decoded) != nil) {
				conversionErr = fmt.Errorf("checkpoint raw data must be a base64 string or null")
			}
			return json.RawMessage(decoded)
		}
		if value == nil {
			return nil
		}
		encoded, _ := json.Marshal([]byte(value))
		return encoded
	}
	user := func(value *UserTurn) *UserTurn {
		if value == nil {
			return nil
		}
		copied := *value
		copied.Workspace = raw(value.Workspace)
		copied.Roster = raw(value.Roster)
		return &copied
	}
	if cp.History != nil {
		cp.History = append([]Entry{}, cp.History...)
	}
	for i, entry := range cp.History {
		cp.History[i].User = user(entry.User)
		if entry.Assistant != nil {
			response := *entry.Assistant
			if response.Native.Items != nil {
				response.Native.Items = append([]json.RawMessage{}, response.Native.Items...)
			}
			for j, item := range response.Native.Items {
				response.Native.Items[j] = raw(item)
			}
			cp.History[i].Assistant = &response
		}
		if entry.Tool != nil {
			result := *entry.Tool
			result.Outcome.Data = raw(result.Outcome.Data)
			cp.History[i].Tool = &result
		}
	}
	cp.PendingSubmission = user(cp.PendingSubmission)
	if cp.GC.Inherited != nil {
		inherited := make(map[string]ToolOutcome, len(cp.GC.Inherited))
		for id, outcome := range cp.GC.Inherited {
			outcome.Data = raw(outcome.Data)
			inherited[id] = outcome
		}
		cp.GC.Inherited = inherited
	}
	return cp, conversionErr
}
