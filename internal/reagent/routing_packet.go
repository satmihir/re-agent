package reagent

import (
	"encoding/json"
	"fmt"
	"strings"
)

// v0 §10 amendment (2026-10-02): noncritical evidence and, when the packet
// would not otherwise fit, earlier user requests get previews.
const routingPreviewBytes = 512

type routingTruncation struct {
	Entry         int    `json:"entry"`
	Field         string `json:"field"`
	OriginalBytes int    `json:"original_bytes"`
}

type routingState struct {
	Workspace           string              `json:"workspace"`
	Mode                string              `json:"mode"`
	PlanMode            bool                `json:"plan_mode"`
	CurrentModel        string              `json:"current_model"`
	CurrentEffort       string              `json:"current_effort"`
	ProjectInstructions *string             `json:"project_instructions,omitempty"`
	Critical            []transcriptEntry   `json:"user_requests_and_summaries"`
	Plan                *transcriptEntry    `json:"latest_complete_plan,omitempty"`
	Recent              []transcriptEntry   `json:"recent_evidence"`
	OmittedEarlier      int                 `json:"omitted_earlier_entries"`
	OmittedRequests     int                 `json:"omitted_earlier_user_requests,omitempty"`
	OmittedNative       int                 `json:"omitted_native_items"`
	Truncations         []routingTruncation `json:"previews,omitempty"`
}

type routingPacket struct {
	State           string
	OmittedEarlier  int
	OmittedRequests int
	OmittedNative   int
	Truncations     []routingTruncation
}

func buildRoutingPacket(cfg Config, history []Entry, planMode bool, routes []jevRoute) (routingPacket, error) {
	entries, nativeItems, err := visibleHistory(history)
	if err != nil {
		return routingPacket{}, err
	}
	state := routingState{
		Workspace: cfg.WorkspacePath, Mode: cfg.Registry.Mode().String(), PlanMode: planMode,
		CurrentModel: cfg.Model, CurrentEffort: cfg.ReasoningEffort, ProjectInstructions: cfg.ProjectInstructions,
		OmittedNative: nativeItems,
	}
	critical := make(map[int]bool)
	hasUser, lastAssistant := false, len(entries)
	for i, entry := range entries {
		if entry.Kind == EntryUser && entry.User.Source == "" || entry.Kind == EntrySummary {
			state.Critical = append(state.Critical, entry)
			critical[i] = true
		}
		if entry.User != nil && entry.User.Source == "" && strings.TrimSpace(entry.User.Text) != "" {
			hasUser = true
		}
		if entry.Assistant != nil {
			lastAssistant = i
			if _, _, found := findPlan(blockText(*history[i].Assistant, BlockText)); found {
				copy := entry
				state.Plan = &copy
			}
		}
	}
	if !hasUser {
		return routingPacket{}, fmt.Errorf("routing objective is missing")
	}
	if state.Plan != nil {
		critical[state.Plan.Number-1] = true
	}
	for i, entry := range entries {
		if critical[i] {
			continue
		}
		if i < lastAssistant {
			state.OmittedEarlier++
			continue
		}
		preview, markers := routingPreview(entry)
		state.Recent = append(state.Recent, preview)
		state.Truncations = append(state.Truncations, markers...)
	}
	encode := func(state routingState) (string, error) {
		body, err := json.Marshal(state)
		if err != nil {
			return "", fmt.Errorf("encode routing state: %w", err)
		}
		if _, err := encodeJevRequest(string(body), routes); err != nil {
			return "", err
		}
		return string(body), nil
	}
	text, err := encode(state)
	// v0 §10 amendment (2026-10-02): earlier user requests yield before the
	// packet is refused, oldest first: previews, then omission. The latest
	// request and every summary stay whole.
	latest := -1
	for i, entry := range state.Critical {
		if entry.Kind == EntryUser {
			latest = i
		}
	}
	recent, previewed := state.Truncations, []routingTruncation(nil)
	for i := 0; err != nil && i < latest; i++ {
		if state.Critical[i].Kind != EntryUser {
			continue
		}
		preview, markers := routingPreview(state.Critical[i])
		if len(markers) == 0 {
			continue
		}
		state.Critical = append([]transcriptEntry(nil), state.Critical...)
		state.Critical[i] = preview
		previewed = append(previewed, markers...)
		state.Truncations = append(append([]routingTruncation(nil), previewed...), recent...)
		text, err = encode(state)
	}
	for err != nil {
		oldest := -1
		for i := 0; i < latest && oldest < 0; i++ {
			if state.Critical[i].Kind == EntryUser {
				oldest = i
			}
		}
		if oldest < 0 {
			break
		}
		number := state.Critical[oldest].Number
		state.Critical = append(append([]transcriptEntry(nil), state.Critical[:oldest]...), state.Critical[oldest+1:]...)
		kept := state.Truncations[:0:0]
		for _, marker := range state.Truncations {
			if marker.Entry != number {
				kept = append(kept, marker)
			}
		}
		state.Truncations = kept
		state.OmittedRequests++
		latest--
		text, err = encode(state)
	}
	if err != nil {
		return routingPacket{}, fmt.Errorf("critical routing state or latest segment does not fit: %w", err)
	}
	// Keep the newest contiguous noncritical evidence that fits, not an arbitrary last-N slice.
	for i := lastAssistant - 1; i >= 0; i-- {
		if critical[i] {
			continue
		}
		preview, markers := routingPreview(entries[i])
		candidate := state
		candidate.Recent = append([]transcriptEntry{preview}, state.Recent...)
		candidate.Truncations = append(append([]routingTruncation(nil), state.Truncations...), markers...)
		candidate.OmittedEarlier--
		candidateText, err := encode(candidate)
		if err != nil {
			break
		}
		state, text = candidate, candidateText
	}
	return routingPacket{State: text, OmittedEarlier: state.OmittedEarlier, OmittedRequests: state.OmittedRequests, OmittedNative: nativeItems, Truncations: state.Truncations}, nil
}

func routingPreview(entry transcriptEntry) (transcriptEntry, []routingTruncation) {
	var markers []routingTruncation
	preview := func(text, field string) string {
		if len(text) <= routingPreviewBytes {
			return text
		}
		markers = append(markers, routingTruncation{Entry: entry.Number, Field: field, OriginalBytes: len(text)})
		return truncateUTF8(text, routingPreviewBytes)
	}
	if entry.User != nil {
		copy := *entry.User
		copy.Text = preview(copy.Text, "user.text")
		entry.User = &copy
	}
	if entry.Assistant != nil {
		copy := *entry.Assistant
		copy.Blocks = append([]OutputBlock(nil), copy.Blocks...)
		for i := range copy.Blocks {
			block := &copy.Blocks[i]
			block.Text = preview(block.Text, fmt.Sprintf("assistant.blocks.%d.text", i))
			if block.Call != nil {
				call := *block.Call
				call.Arguments = preview(call.Arguments, fmt.Sprintf("assistant.blocks.%d.arguments", i))
				block.Call = &call
			}
		}
		entry.Assistant = &copy
	}
	if entry.Tool != nil {
		copy := *entry.Tool
		copy.Outcome.Message = preview(copy.Outcome.Message, "tool.message")
		if len(copy.Outcome.Data) > routingPreviewBytes {
			// A JSON string preview is deliberately not a fabricated structured tool result.
			copy.Outcome.Data, _ = json.Marshal(preview(string(copy.Outcome.Data), "tool.data"))
			copy.Outcome.Truncated = true
		}
		entry.Tool = &copy
	}
	if entry.Shell != nil {
		copy := *entry.Shell
		copy.Command = preview(copy.Command, "shell.command")
		copy.Output = preview(copy.Output, "shell.output")
		if len(markers) != 0 {
			copy.OutputTruncated = true
		}
		entry.Shell = &copy
	}
	return entry, markers
}
