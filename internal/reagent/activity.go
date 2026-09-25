package reagent

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"
)

type mark int

const (
	markOK mark = iota
	markFailed
	markUncertain
	markSkipped
)

type activity struct {
	mark    mark
	tool    string
	target  string
	result  string
	preview []string
}

func describeActivity(call ToolCall, outcome ToolOutcome) activity {
	a := activity{mark: markOK, tool: sanitize(call.Name), target: callTarget(call)}
	if outcome.Code == "not_executed" {
		a.mark, a.result = markSkipped, "not run: "+sanitize(outcome.Message)
		return a
	}
	if !outcome.OK {
		a.mark, a.result = markFailed, truncateWidth(sanitize(outcome.Code+": "+outcome.Message), 120)
		if call.Name != "exec" {
			return a
		}
	}
	switch call.Name {
	case "read_file":
		var args readFileArgs
		var result readFileResult
		if json.Unmarshal([]byte(call.Arguments), &args) != nil || json.Unmarshal(outcome.Data, &result) != nil {
			return a
		}
		a.target = sanitize(args.Path)
		if len(result.Lines) == 0 {
			a.result = "empty"
			return a
		}
		if result.Lines[0].Number == 1 && result.EOF {
			a.result = plural(len(result.Lines), "line", "lines")
			return a
		}
		a.result = fmt.Sprintf("lines %d-%d of %d", result.Lines[0].Number, result.Lines[len(result.Lines)-1].Number, result.TotalLines)
	case "list_files":
		var args listFilesArgs
		var result listFilesResult
		if json.Unmarshal([]byte(call.Arguments), &args) != nil || json.Unmarshal(outcome.Data, &result) != nil {
			return a
		}
		a.target, a.result = sanitize(args.Path), plural(len(result.Entries), "entry", "entries")
		if result.NextOffset != nil {
			a.result += ", more"
		}
	case "search_text":
		var args searchTextArgs
		var result searchTextResult
		if json.Unmarshal([]byte(call.Arguments), &args) != nil || json.Unmarshal(outcome.Data, &result) != nil {
			return a
		}
		a.target = "\"" + oneRow(args.Query) + "\" in " + sanitize(args.Path)
		if len(result.Matches) == 0 {
			a.result = "no matches"
		} else {
			files := map[string]bool{}
			for _, m := range result.Matches {
				files[m.Path] = true
			}
			a.result = fmt.Sprintf("%s in %s", plural(len(result.Matches), "match", "matches"), plural(len(files), "file", "files"))
		}
		if !result.Complete {
			a.result += ", incomplete"
		}
	case "edit_file":
		var args editFileArgs
		var result editFileResult
		if json.Unmarshal([]byte(call.Arguments), &args) != nil || json.Unmarshal(outcome.Data, &result) != nil {
			return a
		}
		a.target = sanitize(args.Path)
		if !result.Changed {
			a.result = "no change"
			return a
		}
		old, new := changedLines(args.OldText, args.NewText)
		a.result = fmt.Sprintf("+%d -%d", len(new), len(old))
		a.preview = previewLines(old, new)
	case "exec":
		var args execArgs
		var result execResult
		if json.Unmarshal([]byte(call.Arguments), &args) != nil || json.Unmarshal(outcome.Data, &result) != nil {
			return a
		}
		a.target = commandText(args)
		d := formatElapsed(time.Duration(result.DurationMS) * time.Millisecond)
		switch outcome.Code {
		case "timeout":
			a.mark, a.result = markUncertain, "timed out after "+d+"; effects unknown"
		case "cancelled":
			a.mark, a.result = markUncertain, "cancelled; effects unknown"
		default:
			if result.Signal != nil {
				a.mark, a.result = markFailed, "signal: "+sanitize(*result.Signal)+" in "+d
			} else if result.ExitCode != nil {
				a.result = fmt.Sprintf("exit %d in %s", *result.ExitCode, d)
			}
		}
	case "echo":
		var args echoArgs
		if json.Unmarshal([]byte(call.Arguments), &args) == nil {
			a.target = fmt.Sprintf("%q", sanitize(args.Text))
		}
	}
	return a
}

func (a activity) render(styled bool, columns int) string {
	marker := map[mark]string{markOK: "✓", markFailed: "✗", markUncertain: "!", markSkipped: "–"}[a.mark]
	prefix := "  " + marker + " " + a.tool + " "
	target, result := a.target, a.result
	limit := columns
	if limit == 0 {
		target = truncateWidth(target, 160)
	} else {
		// Keep the marker and tool intact. Spend available cells on the target,
		// then shorten the result only when necessary.
		available := limit - displayWidth(prefix)
		if result != "" {
			available -= displayWidth(" → ")
		}
		if available < 1 {
			target, result = "", ""
		} else if displayWidth(target)+displayWidth(result) > available {
			targetBudget := available - min(displayWidth(result), available/3)
			if targetBudget < 1 {
				targetBudget = 1
			}
			target = truncateWidth(target, targetBudget)
			resultBudget := available - displayWidth(target)
			if resultBudget < 1 {
				result = ""
			} else {
				result = truncateWidth(result, resultBudget)
			}
		}
	}
	line := prefix + target
	if result != "" {
		line += " → " + result
	}
	if styled {
		line = "  " + styleMark(a.mark, marker) + " " + a.tool + " " + target
		if result != "" {
			line += ansiDim + " → " + result + ansiReset
		}
	}
	var out strings.Builder
	out.WriteString(line + "\n")
	for _, preview := range a.preview {
		preview = truncateWidth(preview, previewWidth(columns))
		if styled && strings.HasPrefix(preview, "- ") {
			preview = ansiRed + preview + ansiReset
		} else if styled && strings.HasPrefix(preview, "+ ") {
			preview = ansiGreen + preview + ansiReset
		}
		out.WriteString("      " + preview + "\n")
	}
	return out.String()
}

func previewWidth(columns int) int {
	if columns == 0 {
		return 160
	}
	return max(columns-6, 1)
}

func callTarget(call ToolCall) string {
	switch call.Name {
	case "read_file":
		var args readFileArgs
		if json.Unmarshal([]byte(call.Arguments), &args) == nil {
			return sanitize(args.Path)
		}
	case "list_files":
		var args listFilesArgs
		if json.Unmarshal([]byte(call.Arguments), &args) == nil {
			return sanitize(args.Path)
		}
	case "search_text":
		var args searchTextArgs
		if json.Unmarshal([]byte(call.Arguments), &args) == nil {
			return "\"" + oneRow(args.Query) + "\" in " + sanitize(args.Path)
		}
	case "edit_file":
		var args editFileArgs
		if json.Unmarshal([]byte(call.Arguments), &args) == nil {
			return sanitize(args.Path)
		}
	case "exec":
		var args execArgs
		if json.Unmarshal([]byte(call.Arguments), &args) == nil {
			return commandText(args)
		}
	case "echo":
		var args echoArgs
		if json.Unmarshal([]byte(call.Arguments), &args) == nil {
			return "\"" + sanitize(args.Text) + "\""
		}
	}
	return truncateWidth(sanitize(argumentSummary(call.Arguments)), 160)
}
func recapLine(call ToolCall, outcome ToolOutcome) string {
	if call.Name == "edit_file" && outcome.Effect != EffectNone {
		return "changed " + callTarget(call)
	}
	if call.Name == "exec" && outcome.Effect != EffectNone {
		return "ran " + callTarget(call)
	}
	return ""
}
func plural(n int, singular, pluralForm string) string {
	if n == 1 {
		return "1 " + singular
	}
	return fmt.Sprintf("%d %s", n, pluralForm)
}
func changedLines(old, new string) ([]string, []string) {
	a, b := strings.Split(old, "\n"), strings.Split(new, "\n")
	for len(a) > 0 && len(b) > 0 && a[0] == b[0] {
		a, b = a[1:], b[1:]
	}
	for len(a) > 0 && len(b) > 0 && a[len(a)-1] == b[len(b)-1] {
		a, b = a[:len(a)-1], b[:len(b)-1]
	}
	return a, b
}
func previewLines(old, new []string) []string {
	var out []string
	for i, s := range old {
		if i == 6 {
			out = append(out, "… "+plural(len(old)-i, "more line", "more lines"))
			break
		}
		out = append(out, "- "+sanitize(strings.ReplaceAll(s, "\t", "    ")))
	}
	for i, s := range new {
		if i == 6 {
			out = append(out, "… "+plural(len(new)-i, "more line", "more lines"))
			break
		}
		out = append(out, "+ "+sanitize(strings.ReplaceAll(s, "\t", "    ")))
	}
	return out
}

// v0 §10 amendment (2026-09-25): status, activity, and recap stay on one row.
func oneRow(text string) string {
	return strings.ReplaceAll(sanitize(text), "\n", "↵")
}

func commandText(a execArgs) string {
	parts := make([]string, len(a.Argv))
	for i, s := range a.Argv {
		hasNewline := strings.Contains(s, "\n")
		s = oneRow(s)
		if hasNewline || s == "" || strings.ContainsAny(s, " $`\\\"'*?[]{}()<>|&;") {
			s = "'" + strings.ReplaceAll(s, "'", "'\\''") + "'"
		}
		parts[i] = s
	}
	text := strings.Join(parts, " ")
	if a.Cwd != "" && a.Cwd != "." {
		text += " in " + oneRow(a.Cwd)
	}
	return text
}
