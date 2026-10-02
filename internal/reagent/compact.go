package reagent

import (
	_ "embed"
	"fmt"
	"strings"
)

//go:embed compact.txt
var compactPrompt string

// summaryPreamble keeps a handoff distinct from the user's own instructions.
const summaryPreamble = "Summary of the conversation so far, written when it was compacted:"

// compactRequest adds a temporary user entry without changing the session history.
func compactRequest(s *Session, focus, runID string) ModelRequest {
	prompt := compactPrompt
	if focus = strings.TrimSpace(focus); focus != "" {
		prompt = strings.TrimSuffix(prompt, "\n") + "\n\nFocus: " + focus
	}
	history := append([]Entry(nil), s.requestHistory()...)
	history = append(history, Entry{Kind: EntryUser, User: &UserTurn{Text: prompt}})
	return BuildContext(s.cfg, RequestScope{SessionID: s.ID, RunID: runID, Step: 1}, history)
}

// compactText accepts only a text-only response and preserves block order.
func compactText(resp ModelResponse) (string, error) {
	var parts []string
	for _, block := range resp.Blocks {
		switch block.Kind {
		case BlockText:
			parts = append(parts, block.Text)
		case BlockToolCall:
			return "", fmt.Errorf("summary response contains a tool call")
		case BlockRefusal:
			return "", fmt.Errorf("summary response contains a refusal")
		default:
			return "", fmt.Errorf("summary response contains unsupported block kind %q", block.Kind)
		}
	}
	text := strings.TrimSpace(strings.Join(parts, "\n"))
	if text == "" {
		return "", fmt.Errorf("summary response is empty")
	}
	return text, nil
}
