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
		a.mark, a.result = markFailed, sanitize(outcome.Code+": "+outcome.Message)
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
			a.result = plural(len(result.Lines), "line")
			return a
		}
		a.result = fmt.Sprintf("lines %d-%d of %d", result.Lines[0].Number, result.Lines[len(result.Lines)-1].Number, result.TotalLines)
	case "list_files":
		var args listFilesArgs
		var result listFilesResult
		if json.Unmarshal([]byte(call.Arguments), &args) != nil || json.Unmarshal(outcome.Data, &result) != nil {
			return a
		}
		a.target, a.result = sanitize(args.Path), plural(len(result.Entries), "entry")
		if result.NextOffset != nil {
			a.result += ", more"
		}
	case "search_text":
		var args searchTextArgs
		var result searchTextResult
		if json.Unmarshal([]byte(call.Arguments), &args) != nil || json.Unmarshal(outcome.Data, &result) != nil {
			return a
		}
		a.target = "\"" + sanitize(args.Query) + "\" in " + sanitize(args.Path)
		if len(result.Matches) == 0 {
			a.result = "no matches"
		} else {
			files := map[string]bool{}
			for _, m := range result.Matches {
				files[m.Path] = true
			}
			a.result = fmt.Sprintf("%s in %s", plural(len(result.Matches), "match"), plural(len(files), "file"))
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
		var args struct {
			Text string `json:"text"`
		}
		if json.Unmarshal([]byte(call.Arguments), &args) == nil {
			a.target = fmt.Sprintf("%q", sanitize(args.Text))
		}
	}
	return a
}

func (a activity) render(styled bool, columns int) string {
	marker := map[mark]string{markOK: "✓", markFailed: "✗", markUncertain: "!", markSkipped: "–"}[a.mark]
	line := "  " + marker + " " + a.tool + " " + a.target
	if a.result != "" {
		line += " → " + a.result
	}
	if columns > 0 {
		line = truncateWidth(line, columns)
	}
	if styled {
		line = "  " + styleMark(a.mark, marker) + line[len("  "+marker):]
	}
	var out strings.Builder
	out.WriteString(line + "\n")
	for _, p := range a.preview {
		out.WriteString("      " + p + "\n")
	}
	return out.String()
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
			return "\"" + sanitize(args.Query) + "\" in " + sanitize(args.Path)
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
		var args struct {
			Text string `json:"text"`
		}
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
func plural(n int, noun string) string {
	if n == 1 {
		return "1 " + noun
	}
	return fmt.Sprintf("%d %ss", n, noun)
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
			out = append(out, fmt.Sprintf("… %d more lines", len(old)-i))
			break
		}
		out = append(out, "- "+sanitize(strings.ReplaceAll(s, "\t", "    ")))
	}
	for i, s := range new {
		if i == 6 {
			out = append(out, fmt.Sprintf("… %d more lines", len(new)-i))
			break
		}
		out = append(out, "+ "+sanitize(strings.ReplaceAll(s, "\t", "    ")))
	}
	return out
}
func commandText(a execArgs) string {
	parts := make([]string, len(a.Argv))
	for i, s := range a.Argv {
		s = sanitize(s)
		if s == "" || strings.ContainsAny(s, " $`\\\"'*?[]{}()<>|&;") {
			s = "'" + strings.ReplaceAll(s, "'", "'\\''") + "'"
		}
		parts[i] = s
	}
	text := strings.Join(parts, " ")
	if a.Cwd != "" && a.Cwd != "." {
		text += " in " + sanitize(a.Cwd)
	}
	return text
}
