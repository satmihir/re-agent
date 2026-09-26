package reagent

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

// contextPart is one labelled share of an encoded request. A subset part is
// already counted in the part above it, so it is not added to the total.
type contextPart struct {
	label  string
	bytes  int
	subset bool
}

// contextBreakdown is what the next request of a session is made of (v0 §10
// amendment of 2026-09-24). It is measured, never sent.
type contextBreakdown struct {
	parts     []contextPart
	total     int
	overLimit bool
	lastUsage Usage
}

// measureContext sizes each part of the request the session would send next,
// as the bytes it occupies in the encoded body.
//
// The total comes from the provider's own encoder, so it is the request rather
// than an estimate of one. Whatever the parts do not account for is the JSON
// around them: keys, roles, call ids, and punctuation.
func measureContext(cfg Config, history []Entry) (contextBreakdown, error) {
	req := BuildContext(cfg, RequestScope{}, history)
	var b contextBreakdown
	body, err := encodeRequest(cfg, req)
	var modelErr *ModelError
	switch {
	case errors.As(err, &modelErr) && modelErr.Status == StatusLimitExceeded:
		// The case most worth describing is the one that can no longer be sent.
		b.overLimit = true
	case err != nil:
		return contextBreakdown{}, err
	default:
		b.total = len(body)
	}

	tools := 0
	for _, spec := range req.Tools {
		tools += encodedSize(spec.Name) + encodedSize(spec.Description) + encodedSize(spec.InputSchema)
	}
	// The conversation's own parts come first in a fixed order, then each
	// tool's results in the order the tools were first used.
	sizes := map[string]int{}
	order := []string{"your messages", "model reasoning", "model tool calls", "model text"}
	add := func(label string, n int) {
		if _, seen := sizes[label]; !seen && strings.HasSuffix(label, " results") {
			order = append(order, label)
		}
		sizes[label] += n
	}

	// A read is stale once a later edit changed the same file, and a repeat
	// when an earlier result had exactly the same bytes. Edits made through
	// exec are invisible here.
	lastEdit := map[string]int{}
	for i, entry := range history {
		if ref, ok := fileResult(entry, "edit_file"); ok && ref.Changed {
			lastEdit[ref.Path] = i
		}
	}
	stale, repeated := 0, 0
	seenReads := map[string]bool{}

	for i, entry := range history {
		switch entry.Kind {
		case EntryUser:
			add("your messages", encodedSize(entry.User.Text))
		case EntryAssistant:
			for _, item := range entry.Assistant.Native.Items {
				var kind struct {
					Type string `json:"type"`
				}
				_ = json.Unmarshal(item, &kind)
				add(nativeItemLabel(kind.Type), encodedSize(item))
			}
		case EntryTool:
			// Both encoders send an outcome as a JSON string holding its JSON.
			outcome, err := json.Marshal(entry.Tool.Outcome)
			if err != nil {
				return contextBreakdown{}, err
			}
			n := encodedSize(string(outcome))
			add(sanitize(entry.Tool.Name)+" results", n)
			ref, ok := fileResult(entry, "read_file")
			switch {
			case !ok:
			case lastEdit[ref.Path] > i:
				stale += n
			case seenReads[string(outcome)]:
				repeated += n
			}
			seenReads[string(outcome)] = true
		}
	}

	b.parts = []contextPart{{label: "instructions", bytes: encodedSize(req.Instructions)}, {label: "tool definitions", bytes: tools}}
	for _, label := range order {
		if _, seen := sizes[label]; !seen {
			continue
		}
		b.parts = append(b.parts, contextPart{label: label, bytes: sizes[label]})
		if label != "read_file results" {
			continue
		}
		if stale > 0 {
			b.parts = append(b.parts, contextPart{label: "file edited since", bytes: stale, subset: true})
		}
		if repeated > 0 {
			b.parts = append(b.parts, contextPart{label: "exact repeat", bytes: repeated, subset: true})
		}
	}
	if !b.overLimit {
		b.parts = append(b.parts, contextPart{label: "JSON structure", bytes: b.total - b.counted()})
	}

	for i := len(history) - 1; i >= 0; i-- {
		if history[i].Kind == EntryAssistant {
			b.lastUsage = history[i].Assistant.Usage
			break
		}
	}
	return b, nil
}

// fileResult reads the path out of a successful result of the named tool.
func fileResult(entry Entry, tool string) (ref struct {
	Path    string `json:"path"`
	Changed bool   `json:"changed"`
}, ok bool) {
	if entry.Kind != EntryTool || entry.Tool.Name != tool || !entry.Tool.Outcome.OK {
		return ref, false
	}
	return ref, json.Unmarshal(entry.Tool.Outcome.Data, &ref) == nil && ref.Path != ""
}

// encodedSize is the length of v once encoded as JSON. Every value measured
// here has already been encoded once by the request encoder, or is a string,
// so encoding cannot fail.
func encodedSize(v any) int {
	encoded, _ := json.Marshal(v)
	return len(encoded)
}

// counted is the sum of the parts that are not a subset of another.
func (b contextBreakdown) counted() int {
	sum := 0
	for _, part := range b.parts {
		if !part.subset {
			sum += part.bytes
		}
	}
	return sum
}

// render lays the breakdown out for /context. Shares are of bytes: providers
// report tokens only per request, and reasoning is sent encrypted, so bytes
// are what can be attributed.
func (b contextBreakdown) render() string {
	var out strings.Builder
	whole := b.total
	if b.overLimit {
		whole = b.counted()
		fmt.Fprintf(&out, "next request  over the %s limit; /reset to continue\n", formatBytes(MaxRequestBytes))
	} else {
		fmt.Fprintf(&out, "next request  %s, %.0f%% of the %s limit\n",
			formatBytes(b.total), 100*float64(b.total)/MaxRequestBytes, formatBytes(MaxRequestBytes))
	}
	if b.lastUsage.Known {
		fmt.Fprintf(&out, "last request  %s tokens in (%s cached)\n",
			formatCount(b.lastUsage.InputTokens), formatCount(b.lastUsage.CachedInputTokens))
	}
	for _, part := range b.parts {
		label := "  " + part.label
		if part.subset {
			label = "    " + part.label
		}
		fmt.Fprintf(&out, "%-24s %10s %4.0f%%\n", label, formatBytes(part.bytes), 100*float64(part.bytes)/float64(max(whole, 1)))
	}
	out.WriteString("shares are of request bytes, not tokens")
	return out.String()
}

// formatBytes uses binary units, matching how MaxRequestBytes is defined.
func formatBytes(n int) string {
	switch {
	case n < 1<<10:
		return fmt.Sprintf("%d B", n)
	case n < 1<<20:
		return fmt.Sprintf("%.1f KiB", float64(n)/(1<<10))
	}
	return fmt.Sprintf("%.1f MiB", float64(n)/(1<<20))
}
