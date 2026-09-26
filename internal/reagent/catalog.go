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
)

// modelInfo is one entry of the offered catalog.
//
// Efforts lists exactly what the model accepts; an empty list means the model
// rejects the parameter outright. Effort is what a run uses when this model is
// chosen and nothing else was asked for.
type modelInfo struct {
	ID       string
	Provider string
	Efforts  []string
	Effort   string
	Note     string
}

// modelCatalog is the offered set, in the order it is listed.
//
// It is a fixed list on purpose. Asking each provider what it serves would be
// one more thing that can fail at startup, for a menu that changes a few times
// a year, and --model still accepts any name a provider knows.
var modelCatalog = []modelInfo{
	{ID: "gpt-6-luna", Provider: openaiName, Efforts: openaiEfforts, Effort: "low",
		Note: "cheapest"},
	{ID: "gpt-6-sol", Provider: openaiName, Efforts: openaiEfforts, Effort: "low",
		Note: "most capable, costs most"},
	{ID: "gpt-5.6-luna", Provider: openaiName, Efforts: openaiEfforts, Effort: "low",
		Note: "cheap"},
	{ID: "gpt-5.6-terra", Provider: openaiName, Efforts: openaiEfforts, Effort: "low",
		Note: "more capable, costs more"},
	{ID: "claude-haiku-4-5", Provider: anthropicName, Efforts: nil, Effort: "",
		Note: "cheapest; no effort setting"},
	{ID: "claude-sonnet-5", Provider: anthropicName, Efforts: anthropicEfforts, Effort: "low",
		Note: "more capable, costs more"},
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
	out.WriteString("\nswitch with /model <number or name>; a model change starts a fresh session")
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
