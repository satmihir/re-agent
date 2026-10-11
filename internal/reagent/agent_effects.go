package reagent

import "encoding/json"

const maxAgentEffectsBytes = 8 * 1024

type agentFileEffect struct {
	Workspace string `json:"workspace"`
	Path      string `json:"path"`
	Operation string `json:"operation"`
}

type agentEffects struct {
	Files       []agentFileEffect `json:"files"`
	Omitted     int               `json:"omitted,omitempty"`
	ExecUnknown bool              `json:"exec_unknown"`
}

func summarizeAgentEffects(records []EffectRecord) agentEffects {
	summary := agentEffects{Files: []agentFileEffect{}}
	seen := make(map[agentFileEffect]bool)
	bytes := 128
	for _, record := range records {
		summary.Omitted += record.OmittedPaths
		if record.Tool == "exec" && record.Effect == EffectUnknown {
			summary.ExecUnknown = true
		}
		if record.Path == "" {
			continue
		}
		file := agentFileEffect{Workspace: record.Workspace, Path: record.Path, Operation: record.Operation}
		if seen[file] {
			continue
		}
		seen[file] = true
		// Delivery escapes the record once as JSON and again inside user text.
		raw, _ := json.Marshal(file)
		encoded, _ := json.Marshal(string(raw))
		if bytes+len(encoded)+1 > maxAgentEffectsBytes {
			summary.Omitted++
			continue
		}
		bytes += len(encoded) + 1
		summary.Files = append(summary.Files, file)
	}
	return summary
}
