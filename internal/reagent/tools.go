package reagent

import (
	"bytes"
	"encoding/json"
	"fmt"
	"regexp"
	"sort"
	"strings"
)

var toolNamePattern = regexp.MustCompile(`^[a-z][a-z0-9_]{0,63}$`)

// Registry is the fixed set of tools available for one run. It is built once at
// startup and never changes while the run is in progress.
type Registry struct {
	byName map[string]Tool
	specs  []ToolSpec
}

// NewRegistry validates the given tools and freezes them in name order. Sorting
// keeps the declaration the model sees stable across runs.
func NewRegistry(tools ...Tool) (*Registry, error) {
	r := &Registry{byName: make(map[string]Tool, len(tools))}
	for _, t := range tools {
		spec := t.Spec()
		if !toolNamePattern.MatchString(spec.Name) {
			return nil, fmt.Errorf("invalid tool name %q", spec.Name)
		}
		if _, dup := r.byName[spec.Name]; dup {
			return nil, fmt.Errorf("duplicate tool name %q", spec.Name)
		}
		if !json.Valid(spec.InputSchema) {
			return nil, fmt.Errorf("tool %q has an invalid input schema", spec.Name)
		}
		r.byName[spec.Name] = t
		r.specs = append(r.specs, spec)
	}
	sort.Slice(r.specs, func(i, j int) bool { return r.specs[i].Name < r.specs[j].Name })
	return r, nil
}

// Specs returns the model-visible declarations, sorted by name.
func (r *Registry) Specs() []ToolSpec { return r.specs }

// Lookup finds an active tool by the name the model used.
func (r *Registry) Lookup(name string) (Tool, bool) {
	t, found := r.byName[name]
	return t, found
}

// decodeArgs parses exactly one JSON object into dst, rejecting unknown fields
// and trailing tokens. A failure is an observation the model can correct, so it
// returns an outcome rather than an error (v0 §5).
func decodeArgs(args json.RawMessage, dst any) *ToolOutcome {
	if trimmed := strings.TrimSpace(string(args)); !strings.HasPrefix(trimmed, "{") {
		return failPtr("invalid_arguments", "arguments must be one JSON object")
	}
	dec := json.NewDecoder(bytes.NewReader(args))
	dec.DisallowUnknownFields()
	if err := dec.Decode(dst); err != nil {
		return failPtr("invalid_arguments", err.Error())
	}
	if dec.More() {
		return failPtr("invalid_arguments", "arguments must be one JSON object")
	}
	return nil
}

// okOutcome builds a successful outcome. A marshalling failure is an
// implementation defect, not something the model can act on, so it is an error.
func okOutcome(data any) (ToolOutcome, error) {
	raw, err := json.Marshal(data)
	if err != nil {
		return ToolOutcome{}, fmt.Errorf("marshal tool data: %w", err)
	}
	return ToolOutcome{OK: true, Code: "ok", Data: raw, Effect: EffectNone}, nil
}

// failOutcome builds an expected failure the model is meant to read and react to.
func failOutcome(code, message string) ToolOutcome {
	return ToolOutcome{Code: code, Message: message, Effect: EffectNone}
}

func failPtr(code, message string) *ToolOutcome {
	o := failOutcome(code, message)
	return &o
}
