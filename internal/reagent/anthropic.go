package reagent

import (
	"encoding/json"
	"fmt"
)

// DefaultAnthropicModel is the Anthropic model used when nothing says otherwise.
const DefaultAnthropicModel = "claude-haiku-5-5"

// anthropicMaxTokens is the default output ceiling required by Messages.
const anthropicMaxTokens = 16000

// messagesRequest is the Messages body this adapter sends. The system prompt
// is a block rather than a string so it can carry a cache breakpoint.
type messagesRequest struct {
	Model        string                `json:"model"`
	MaxTokens    int                   `json:"max_tokens"`
	System       []messagesTextBlock   `json:"system,omitempty"`
	Messages     []messagesMessage     `json:"messages"`
	Tools        []messagesTool        `json:"tools,omitempty"`
	ToolChoice   *messagesToolChoice   `json:"tool_choice,omitempty"`
	OutputConfig *messagesOutputConfig `json:"output_config,omitempty"`
}

type messagesTextBlock struct {
	Type         string                `json:"type"`
	Text         string                `json:"text"`
	CacheControl *messagesCacheControl `json:"cache_control,omitempty"`
}

type messagesCacheControl struct {
	Type string `json:"type"`
}

// messagesMessage is one turn. Content is a block list: text blocks for the
// user, the provider's own blocks for the assistant, tool results in between.
type messagesMessage struct {
	Role    string `json:"role"`
	Content any    `json:"content"`
}

type messagesToolResult struct {
	Type         string                `json:"type"`
	ToolUseID    string                `json:"tool_use_id"`
	Content      string                `json:"content"`
	IsError      bool                  `json:"is_error,omitempty"`
	CacheControl *messagesCacheControl `json:"cache_control,omitempty"`
}

// ephemeralCache is the breakpoint marker. It is read-only once built, so one
// value is shared by every block that carries it.
var ephemeralCache = &messagesCacheControl{Type: "ephemeral"}

type messagesTool struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

type messagesToolChoice struct {
	Type                   string `json:"type"`
	DisableParallelToolUse bool   `json:"disable_parallel_tool_use"`
}

type messagesOutputConfig struct {
	Effort string `json:"effort"`
}

// EncodeAnthropicRequest turns one logical request into the exact bytes sent
// to the Messages API. The live adapter and --show-context both call it, so
// the preview is the request itself (v0 §6.1).
func EncodeAnthropicRequest(req ModelRequest) ([]byte, error) {
	messages, err := encodeAnthropicHistoryWithCache(req.History, req.CacheEntries)
	if err != nil {
		return nil, err
	}

	tools := make([]messagesTool, len(req.Tools))
	for i, spec := range req.Tools {
		tools[i] = messagesTool{Name: spec.Name, Description: spec.Description, InputSchema: spec.InputSchema}
	}

	// Two breakpoints, each marking the end of a prefix worth caching.
	//
	// The first covers the tools and the instructions, which are identical
	// across every run of a session. The second, placed below, covers the
	// conversation so far, which is where the tokens actually are: by the
	// second step the history dwarfs the fixed prefix, and it is the same
	// bytes on every later request. Anthropic caching is explicit, so a prefix
	// without a breakpoint is simply resent at full price.
	system := []messagesTextBlock{{
		Type: "text", Text: req.Instructions, CacheControl: ephemeralCache,
	}}

	// An unconfigured effort omits the parameter rather than sending a value
	// the model may not accept.
	var outputConfig *messagesOutputConfig
	if req.ReasoningEffort != "" {
		outputConfig = &messagesOutputConfig{Effort: req.ReasoningEffort}
	}

	// Keep inherited markers byte-identical, within the provider's four-point
	// limit (one system point and at most three history points).
	if len(req.CacheEntries) < 3 {
		markCachePoint(messages)
	}

	maxTokens := anthropicMaxTokens
	if req.MaxOutputTokens > 0 {
		maxTokens = req.MaxOutputTokens
	}
	// Permit several tool_use blocks per response; dispatch remains sequential.
	body, err := json.Marshal(messagesRequest{
		Model:        req.Model,
		MaxTokens:    maxTokens,
		System:       system,
		Messages:     messages,
		Tools:        tools,
		ToolChoice:   &messagesToolChoice{Type: "auto", DisableParallelToolUse: false},
		OutputConfig: outputConfig,
	})
	if err != nil {
		return nil, fmt.Errorf("cannot encode Anthropic request: invalid JSON data")
	}
	if len(body) > MaxRequestBytes {
		return nil, &ModelError{
			Status: StatusLimitExceeded,
			Message: fmt.Sprintf("request is %d bytes, over the %d byte limit; narrow the task or start a new run",
				len(body), MaxRequestBytes),
		}
	}
	return body, nil
}

// encodeAnthropicHistory expands the transcript into alternating messages.
//
// The differences from the Responses encoding are the shape of a turn, not its
// meaning: an assistant turn is one message whose content is the provider's
// own blocks, kept verbatim (I15), and every tool result answering that turn
// goes into one user message, which is what the API requires. Text that follows
// another user message, such as a ! command or a turn whose request failed,
// joins that message, so user and assistant messages still alternate.
func encodeAnthropicHistory(history []Entry) ([]messagesMessage, error) {
	return encodeAnthropicHistoryWithCache(history, nil)
}

func encodeAnthropicHistoryWithCache(history []Entry, cacheEntries []int) ([]messagesMessage, error) {
	var messages []messagesMessage
	var pendingResults []messagesToolResult

	flushResults := func() {
		if len(pendingResults) > 0 {
			messages = append(messages, messagesMessage{Role: "user", Content: pendingResults})
			pendingResults = nil
		}
	}
	appendUserText := func(text string) {
		flushResults()
		block := messagesTextBlock{Type: "text", Text: text}
		if last := len(messages) - 1; last >= 0 && messages[last].Role == "user" {
			switch blocks := messages[last].Content.(type) {
			case []messagesTextBlock:
				messages[last].Content = append(blocks, block)
				return
			case []messagesToolResult:
				// A run that failed after its tools ran leaves their results
				// last; text joins them, results first, as the API requires.
				mixed := make([]any, 0, len(blocks)+1)
				for _, result := range blocks {
					mixed = append(mixed, result)
				}
				messages[last].Content = append(mixed, block)
				return
			case []any:
				messages[last].Content = append(blocks, block)
				return
			}
		}
		messages = append(messages, messagesMessage{Role: "user", Content: []messagesTextBlock{block}})
	}

	for i, entry := range history {
		switch entry.Kind {
		case EntrySummary:
			// v0 §10 amendment (2026-09-28): adjacent user text joins without changing the summary.
			appendUserText(summaryPreamble + "\n" + entry.Summary.Text)
		case EntryUser:
			appendUserText(entry.User.Text)
			// v0 §6 amendment (2026-09-27): keep both parts in one user message.
			if len(entry.User.Workspace) > 0 {
				appendUserText(workspacePreamble + string(entry.User.Workspace))
			}
			if len(entry.User.Roster) > 0 {
				appendUserText(agentRosterPreamble + string(entry.User.Roster))
			}
			if marker := planText(entry.User.Plan); marker != "" {
				appendUserText(marker)
			}
		case EntryShell:
			text, err := shellCommandText(*entry.Shell)
			if err != nil {
				return nil, err
			}
			appendUserText(text)
		case EntryAssistant:
			flushResults()
			native := entry.Assistant.Native
			if native.Provider != anthropicProvider {
				return nil, fmt.Errorf("assistant turn carries %q items, which this adapter cannot continue from",
					native.Provider)
			}
			if len(native.Items) == 0 {
				return nil, fmt.Errorf("assistant turn carries no provider items to continue from")
			}
			messages = append(messages, messagesMessage{Role: "assistant", Content: native.Items})
		case EntryTool:
			outcome, err := json.Marshal(entry.Tool.Outcome)
			if err != nil {
				return nil, fmt.Errorf("cannot encode tool outcome: invalid JSON data")
			}
			pendingResults = append(pendingResults, messagesToolResult{
				Type:      "tool_result",
				ToolUseID: entry.Tool.CallID,
				Content:   string(outcome),
				IsError:   !entry.Tool.Outcome.OK,
			})
		}
		for _, boundary := range cacheEntries {
			if boundary == i+1 {
				flushResults()
				markCachePoint(messages)
			}
		}
	}
	flushResults()
	return messages, nil
}

// markCachePoint puts a breakpoint on the last block of the last message, so
// everything sent this time is a cached prefix next time.
//
// The final message is always one this encoder built, never a provider block:
// a request is only sent when the transcript ends with a user turn or with the
// results answering the previous turn's calls. An assistant turn is never last,
// because its results are appended before the next request is built.
func markCachePoint(messages []messagesMessage) {
	if len(messages) == 0 {
		return
	}
	switch content := messages[len(messages)-1].Content.(type) {
	case []messagesTextBlock:
		if len(content) > 0 {
			content[len(content)-1].CacheControl = ephemeralCache
		}
	case []messagesToolResult:
		if len(content) > 0 {
			content[len(content)-1].CacheControl = ephemeralCache
		}
	case []any:
		// Mixed content always ends with the text appended after results.
		if text, ok := content[len(content)-1].(messagesTextBlock); ok {
			text.CacheControl = ephemeralCache
			content[len(content)-1] = text
		}
	}
}
