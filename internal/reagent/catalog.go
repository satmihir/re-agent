package reagent

import (
	"fmt"
	"strconv"
	"strings"
)

// Reasoning effort vocabularies. They differ by provider, and within Anthropic
// by model, so a model carries its own list rather than inheriting one.
var (
	openaiEfforts    = []string{"none", "low", "medium", "high", "xhigh", "max"}
	anthropicEfforts = []string{"low", "medium", "high", "xhigh", "max"}
	// gpt-6.1-sol always reasons: its model page lists no "none" (v0 §10
	// amendment (2026-09-29)).
	openaiReasoningEfforts = []string{"low", "medium", "high", "xhigh", "max"}
)

// modelInfo is one entry of the offered catalog.
//
// Efforts lists exactly what the model accepts; an empty list means the model
// rejects the parameter outright. Effort is what a run uses when this model is
// chosen and nothing else was asked for. An empty Verbosity means the
// parameter is never sent.
type modelInfo struct {
	ID            string
	Provider      string
	Efforts       []string
	Effort        string
	Verbosity     string
	Note          string
	ContextWindow int64
}

// modelCatalog is the offered set, in the order it is listed.
//
// It is a fixed list on purpose. Asking each provider what it serves would be
// one more thing that can fail at startup, for a menu that changes a few times
// a year, and --model still accepts any name a provider knows.
var modelCatalog = []modelInfo{
	// v0 §10 amendment (2026-10-02): catalog OpenAI models request low verbosity.
	{ID: "gpt-6-luna", Provider: openaiName, Efforts: openaiEfforts, Effort: "low", Verbosity: "low",
		Note: "cheapest", ContextWindow: 1_050_000},
	// v0 §10 amendment (2026-10-02): restore Sol for Auto; retain 6.1 for selection.
	{ID: "gpt-6-sol", Provider: openaiName, Efforts: openaiEfforts, Effort: "low", Verbosity: "low",
		Note: "more capable, costs more", ContextWindow: 1_050_000},
	{ID: "gpt-5.6-luna", Provider: openaiName, Efforts: openaiEfforts, Effort: "low", Verbosity: "low",
		Note: "cheap"},
	{ID: "gpt-5.6-terra", Provider: openaiName, Efforts: openaiEfforts, Effort: "low", Verbosity: "low",
		Note: "more capable, costs more"},
	{ID: "claude-haiku-4-5", Provider: anthropicName, Efforts: nil, Effort: "",
		Note: "cheapest; no effort setting", ContextWindow: 200_000},
	{ID: "claude-sonnet-5-5", Provider: anthropicName, Efforts: anthropicEfforts, Effort: "low",
		Note: "more capable, costs more", ContextWindow: 1_000_000},
	{ID: "claude-opus-5-5", Provider: anthropicName, Efforts: anthropicEfforts, Effort: "low",
		Note: "most capable, costs most", ContextWindow: 1_000_000},
	{ID: "gpt-6.1-sol", Provider: openaiName, Efforts: openaiReasoningEfforts, Effort: "low", Verbosity: "low",
		Note: "most capable, costs most", ContextWindow: 1_050_000},
}

// findModel returns the catalog entry for an exact model id.
func findModel(id string) (modelInfo, bool) {
	for _, info := range modelCatalog {
		if info.ID == id {
			return info, true
		}
	}
	return modelInfo{}, false
}

// contextWindow returns zero when the catalog does not know the model's size.
func contextWindow(id string) int64 {
	info, _ := findModel(id)
	return info.ContextWindow
}

// accepts reports whether this model takes the given effort value.
func (m modelInfo) accepts(effort string) bool {
	for _, allowed := range m.Efforts {
		if allowed == effort {
			return true
		}
	}
	return false
}

// selectModel resolves what was typed after /model: a list position or an
// exact model id.
func selectModel(choice string) (modelInfo, error) {
	if position, err := strconv.Atoi(choice); err == nil {
		if position < 1 || position > len(modelCatalog) {
			return modelInfo{}, fmt.Errorf("there is no model %d; /model lists them", position)
		}
		return modelCatalog[position-1], nil
	}
	if info, found := findModel(choice); found {
		return info, nil
	}
	return modelInfo{}, fmt.Errorf("no model named %s; /model lists them", choice)
}

// selectEffort resolves what was typed after /effort, against the current
// model's own vocabulary.
func selectEffort(info modelInfo, choice string) (string, error) {
	if len(info.Efforts) == 0 {
		return "", fmt.Errorf("%s takes no reasoning effort setting", info.ID)
	}
	if position, err := strconv.Atoi(choice); err == nil {
		if position < 1 || position > len(info.Efforts) {
			return "", fmt.Errorf("there is no effort %d; /effort lists them", position)
		}
		return info.Efforts[position-1], nil
	}
	if info.accepts(choice) {
		return choice, nil
	}
	return "", fmt.Errorf("%s does not accept %s; it accepts %s",
		info.ID, choice, strings.Join(info.Efforts, ", "))
}

// v0 §10 amendment (2026-09-26): picker rows follow the same catalog and
// wording as the non-terminal listings, but also say which key is missing.
func modelChoices(current string, available map[string]bool) []choice {
	choices := make([]choice, 0, len(modelCatalog))
	for _, info := range modelCatalog {
		row := choice{label: info.ID, detail: info.Provider, note: info.Note}
		if info.ID == current {
			row.note += "  current"
		} else if !available[info.Provider] {
			row.note = "needs " + apiKeyVariable(info.Provider) + "  " + info.Note
			row.disabled = true
		}
		choices = append(choices, row)
	}
	return choices
}

func effortChoices(info modelInfo, current string) []choice {
	choices := make([]choice, 0, len(info.Efforts))
	for _, effort := range info.Efforts {
		row := choice{label: effort}
		if effort == current {
			row.note = "current"
		}
		choices = append(choices, row)
	}
	return choices
}

// renderModels lists the catalog, marking the model in use and any whose
// provider has no credential in this process.
//
// Status is its own column so the markers line up; the note trails, where a
// ragged right edge costs nothing. Which variable is missing is said once,
// underneath, rather than repeated on every row.
func renderModels(current string, available map[string]bool) string {
	var out strings.Builder
	needed := make(map[string]bool)

	for position, info := range modelCatalog {
		status := ""
		switch {
		case info.ID == current:
			status = "current"
		case !available[info.Provider]:
			status = "no key"
			needed[apiKeyVariable(info.Provider)] = true
		}
		line := fmt.Sprintf("  %d  %-17s %-10s %-8s %s", position+1, info.ID, info.Provider, status, info.Note)
		out.WriteString(strings.TrimRight(line, " ") + "\n")
	}

	var missing []string
	for _, provider := range []string{openaiName, anthropicName} {
		if variable := apiKeyVariable(provider); needed[variable] {
			missing = append(missing, variable)
		}
	}
	if len(missing) > 0 {
		fmt.Fprintf(&out, "\nmodels marked no key need %s\n", strings.Join(missing, " or "))
	}
	out.WriteString("\na model change starts a fresh session; /model <number or name> switches")
	return out.String()
}

// renderEfforts lists what the current model accepts.
func renderEfforts(info modelInfo, current string) string {
	if len(info.Efforts) == 0 {
		return info.ID + " takes no reasoning effort setting"
	}
	var out strings.Builder
	fmt.Fprintf(&out, "%s accepts:\n", info.ID)
	for position, effort := range info.Efforts {
		line := fmt.Sprintf("  %d  %s", position+1, effort)
		if effort == current {
			line += "  [current]"
		}
		out.WriteString(line + "\n")
	}
	out.WriteString("\nset with /effort <number or name>")
	return out.String()
}
