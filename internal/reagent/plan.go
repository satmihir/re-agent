package reagent

import "strings"

// v0 §10 amendment (2026-09-28): turn markers leave the cached prefix intact.
const planMarker = `re:agent plan mode is on for this message. Explore and plan; change nothing.
- Read, list, and search freely. edit_file, write_file, delete_file, and exec are refused while plan mode is on, and only the user can end it.
- A request to do the work, made in plan mode, is a request to plan it.
- Answer from the workspace what the workspace can answer. Ask the user about intent and tradeoffs, a few questions at a time, each with the answer you recommend.
- When the plan is complete, give it between a line containing only <plan> and a line containing only </plan>. Make it decision complete, so whoever implements it makes no further choices: the files to change, the behavior, the tests, and how to check the result. Give at most one plan per reply. A revised plan is given in full.`

const planEndedMarker = "re:agent plan mode ended before this message. Tools are available again as the launch mode allows."

// planMarkerFor derives "ended" from the last user entry, so an unused toggle leaves no mark.
func planMarkerFor(history []Entry, on bool) string {
	if on {
		return "on"
	}
	for i := len(history) - 1; i >= 0; i-- {
		if history[i].Kind == EntryUser {
			if history[i].User.Plan == "on" {
				return "ended"
			}
			break
		}
	}
	return ""
}

// planText maps a recorded turn marker to the fixed text sent to either provider.
func planText(marker string) string {
	switch marker {
	case "on":
		return planMarker
	case "ended":
		return planEndedMarker
	}
	return ""
}

// findPlan selects the last opening tag and its exact-line closing tag.
// An unclosed last block is not a finished plan, even if an earlier one closed.
func findPlan(reply string) (start, end int, found bool) {
	open := -1
	for pos := 0; pos < len(reply); {
		lineEnd := strings.IndexByte(reply[pos:], '\n')
		if lineEnd < 0 {
			lineEnd = len(reply)
		} else {
			lineEnd += pos
		}
		switch reply[pos:lineEnd] {
		case "<plan>":
			open = pos
			found = false
		case "</plan>":
			if open >= 0 {
				start, end, found = open, lineEnd, true
				open = -1
			}
		}
		pos = lineEnd + 1
	}
	return start, end, found && open < 0
}

// planContent removes only the tag lines from a block found by findPlan.
func planContent(block string) string {
	return strings.TrimSuffix(strings.TrimSuffix(strings.TrimPrefix(block, "<plan>\n"), "</plan>"), "\n")
}
