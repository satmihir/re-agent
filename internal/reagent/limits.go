package reagent

import (
	"encoding/json"
	"sort"
	"unicode/utf8"
)

// v0's whole bounds policy is these constants (v0 §4). v1 §15.1 restores the
// fuller table of directory, traversal, argument, and token quotas.
const (
	// MaxFileBytes is the largest file snapshot any tool will read.
	MaxFileBytes = 1 << 20
	// MaxResultBytes is the largest encoded tool outcome the model may see.
	MaxResultBytes = 32 << 10
)

// unlimited marks an omitted optional count, which v0 reads as "as many as
// fit" rather than as a fixed maximum (v0 §5).
const unlimited = -1

// fitElements returns the largest n, at most total, whose built result encodes
// within MaxResultBytes.
//
// This is the one place a tool result is trimmed, which is how every
// observation stops at a complete element instead of at a cut in the
// serialized JSON (v0 §4). build must recompute its own continuation metadata
// for the n it is given.
func fitElements(total int, build func(n int) any) int {
	if fitsInResult(build(total)) {
		return total
	}
	// Encoded size grows with n, so the first n that does not fit bounds the rest.
	return sort.Search(total, func(n int) bool { return !fitsInResult(build(n + 1)) })
}

// fitsInResult measures the whole outcome envelope, not just its data, because
// that envelope is what the model actually receives.
func fitsInResult(data any) bool {
	outcome, err := okOutcome(data)
	if err != nil {
		return false
	}
	encoded, err := json.Marshal(outcome)
	return err == nil && len(encoded) <= MaxResultBytes
}

// truncateUTF8 shortens s to at most limit bytes without splitting a rune.
func truncateUTF8(s string, limit int) string {
	if len(s) <= limit {
		return s
	}
	for limit > 0 && !utf8.RuneStart(s[limit]) {
		limit--
	}
	return s[:limit]
}
