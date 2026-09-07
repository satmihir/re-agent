package reagent

import (
	"bytes"
	"encoding/json"
	"fmt"
	"regexp"
	"sort"
	"strings"
)

var (
	toolNamePattern = regexp.MustCompile(`^[a-z][a-z0-9_]{0,63}$`)
	digestPattern   = regexp.MustCompile(`^[0-9a-f]{64}$`)
)

// Registry is the set of tools available for one run. It is built once at
// startup and never changes while the run is in progress.
//
// It keeps the names of tools this build has but this mode does not allow, so
// a call for one of them can be refused for the right reason instead of being
// reported as a tool that does not exist (v1 §10.3).
type Registry struct {
	mode     Mode
	byName   map[string]Tool
	inactive map[string]bool
	specs    []ToolSpec
}

// NewRegistry keeps the tools this mode allows and freezes them in name order.
// Sorting keeps the declaration the model sees stable across runs.
func NewRegistry(mode Mode, tools ...Tool) (*Registry, error) {
	r := &Registry{
		mode:     mode,
		byName:   make(map[string]Tool, len(tools)),
		inactive: make(map[string]bool),
	}
	for _, t := range tools {
		spec := t.Spec()
		switch {
		case !toolNamePattern.MatchString(spec.Name):
			return nil, fmt.Errorf("invalid tool name %q", spec.Name)
		case r.known(spec.Name):
			return nil, fmt.Errorf("duplicate tool name %q", spec.Name)
		case !json.Valid(spec.InputSchema):
			return nil, fmt.Errorf("tool %q has an invalid input schema", spec.Name)
		}
		if !mode.allows(spec.Effect) {
			// Absent from the declaration the model sees, but remembered.
			r.inactive[spec.Name] = true
			continue
		}
		r.byName[spec.Name] = t
		r.specs = append(r.specs, spec)
	}
	sort.Slice(r.specs, func(i, j int) bool { return r.specs[i].Name < r.specs[j].Name })
	return r, nil
}

// Mode is the authority this registry was built for.
func (r *Registry) Mode() Mode { return r.mode }

// Specs returns the model-visible declarations, sorted by name.
func (r *Registry) Specs() []ToolSpec { return r.specs }

// Lookup finds an active tool by the name the model used.
func (r *Registry) Lookup(name string) (Tool, bool) {
	t, found := r.byName[name]
	return t, found
}

// known reports whether this build has a tool by that name at all, active or
// not. It is what separates "no such tool" from "not enabled here".
func (r *Registry) known(name string) bool {
	_, active := r.byName[name]
	return active || r.inactive[name]
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

// optionalInt reads an optional count. An omitted field takes def; an explicit
// null is rejected, because v0 tool arguments do not use null to mean absence
// (v0 §5).
func optionalInt(raw json.RawMessage, field string, def, least int) (int, *ToolOutcome) {
	if len(raw) == 0 {
		return def, nil
	}
	if string(raw) == "null" {
		return 0, failPtr("invalid_arguments", field+" must be omitted rather than null")
	}
	var n int
	if err := json.Unmarshal(raw, &n); err != nil {
		return 0, failPtr("invalid_arguments", field+" must be an integer")
	}
	if n < least {
		return 0, failPtr("invalid_arguments", fmt.Sprintf("%s must be at least %d", field, least))
	}
	return n, nil
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
