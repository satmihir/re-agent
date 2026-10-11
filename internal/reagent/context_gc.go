package reagent

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"os"
	"syscall"
)

const (
	contextGCMinTokens   = 32_000
	contextGCFraction    = 0.15
	contextGCInterval    = 16
	contextGCRecentSteps = 3
)

type contextGCDrop struct {
	Reason      string `json:"reason"`
	Step        int    `json:"step"`
	Placeholder string `json:"placeholder"`
}

type contextGCState struct {
	Dropped  map[string]contextGCDrop `json:"dropped,omitempty"`
	Paths    map[string]string        `json:"paths,omitempty"`
	LastPass int                      `json:"last_pass"`
	// Text-context forks need the originals behind inherited recall IDs.
	Inherited map[string]ToolOutcome `json:"inherited,omitempty"`
}

func gcHistory(history []Entry, dropped map[string]contextGCDrop) []Entry {
	if len(dropped) == 0 {
		return history
	}
	view := append([]Entry(nil), history...)
	for i, entry := range view {
		if entry.Tool == nil {
			continue
		}
		drop, ok := dropped[entry.Tool.CallID]
		if !ok {
			continue
		}
		result := *entry.Tool
		result.Outcome = ToolOutcome{Workspace: result.Outcome.Workspace, OK: true, Code: "context_gc", Message: drop.Placeholder, Effect: result.Outcome.Effect}
		view[i].Tool = &result
	}
	return view
}

func (s *Session) gcHistory() []Entry {
	if !s.cfg.ContextGC {
		return s.history
	}
	return gcHistory(s.history, s.gc.Dropped)
}

// Record the identity while the call runs, not by resolving historical aliases later.
func (s *Session) gcCallPath(call *ToolCall) {
	if !s.cfg.ContextGC || s.cfg.Workspace == nil {
		return
	}
	switch call.Name {
	case "read_file", "edit_file", "write_file", "delete_file":
	default:
		return
	}
	var args struct {
		Path string `json:"path"`
	}
	if json.Unmarshal([]byte(call.Arguments), &args) != nil || args.Path == "" {
		return
	}
	abs, bad := s.cfg.Workspace.resolve(args.Path)
	if bad != nil {
		return
	}
	if s.gc.Paths == nil {
		s.gc.Paths = make(map[string]string)
	}
	s.gc.Paths[call.CallID] = missingPath(abs)
}

type gcResult struct {
	result  *ToolResult
	step    int
	path    string
	read    *readFileResult
	execKey string
}

func gcResults(history []Entry, paths map[string]string) ([]gcResult, int, bool) {
	calls := make(map[string]*ToolCall)
	pending := make(map[string]bool)
	var results []gcResult
	step := 0
	for _, entry := range history {
		if len(pending) != 0 && entry.Kind != EntryTool {
			return nil, step, false
		}
		if entry.Assistant != nil {
			if len(pending) != 0 {
				return nil, step, false
			}
			step++
			for _, call := range toolCalls(*entry.Assistant) {
				calls[call.CallID] = call
				pending[call.CallID] = true
			}
		}
		if entry.Tool == nil {
			continue
		}
		t := entry.Tool
		call := calls[t.CallID]
		if call == nil || call.Name != t.Name || !pending[t.CallID] {
			return nil, step, false
		}
		delete(pending, t.CallID)
		if !t.Outcome.OK || t.Outcome.Effect == EffectUnknown {
			continue
		}
		r := gcResult{result: t, step: step, path: paths[t.CallID]}
		switch t.Name {
		case "read_file":
			var read readFileResult
			if r.path == "" || json.Unmarshal(t.Outcome.Data, &read) != nil || !digestPattern.MatchString(read.SHA256) || len(read.Lines) == 0 {
				continue
			}
			valid := true
			for j, line := range read.Lines {
				if line.Number < 1 || (j > 0 && line.Number != read.Lines[j-1].Number+1) {
					valid = false
				}
			}
			if !valid {
				continue
			}
			r.read = &read
		case "exec":
			var a execArgs
			if decodeArgs(json.RawMessage(call.Arguments), &a) != nil || len(a.Argv) == 0 || a.Cwd == "" {
				continue
			}
			key, _ := json.Marshal(struct {
				Workspace string
				Argv      []string
				Then      [][]string
				Cwd       string
			}{t.Outcome.Workspace, a.Argv, a.Then, a.Cwd})
			r.execKey = string(key)
		case "edit_file", "write_file", "delete_file":
			if r.path == "" || t.Outcome.Effect != EffectApplied {
				continue
			}
		default:
			continue
		}
		results = append(results, r)
	}
	return results, step, len(pending) == 0
}

func gcCandidates(history []Entry, state contextGCState, digest func(string) string) (map[string]contextGCDrop, int) {
	results, steps, settled := gcResults(history, state.Paths)
	if !settled {
		return nil, steps
	}
	drops := make(map[string]contextGCDrop)
	current := make(map[string]string)
	for i, old := range results {
		id := old.result.CallID
		if _, ok := state.Dropped[id]; ok || old.step > steps-contextGCRecentSteps {
			continue
		}
		reason, at := "", 0
		for _, later := range results[i+1:] {
			if old.read != nil && later.path == old.path {
				if later.read != nil && later.read.Lines[0].Number <= old.read.Lines[0].Number && later.read.Lines[len(later.read.Lines)-1].Number >= old.read.Lines[len(old.read.Lines)-1].Number {
					reason, at = "superseded", later.step
					break
				}
				if later.result.Name == "edit_file" || later.result.Name == "write_file" || later.result.Name == "delete_file" {
					reason, at = "stale", later.step
					break
				}
			}
			if old.execKey != "" && old.execKey == later.execKey {
				reason, at = "re-run", later.step
				break
			}
		}
		if reason == "" && old.read != nil && digest != nil {
			now, checked := current[old.path]
			if !checked {
				now = digest(old.path)
				current[old.path] = now
			}
			if now != "" && now != old.read.SHA256 {
				reason, at = "stale", steps
			}
		}
		if reason == "" {
			continue
		}
		target := "exec"
		if old.read != nil {
			target = fmt.Sprintf("read_file %s:%d-%d · digest %.8s", old.read.Path, old.read.Lines[0].Number, old.read.Lines[len(old.read.Lines)-1].Number, old.read.SHA256)
		}
		drops[id] = contextGCDrop{Reason: reason, Step: at, Placeholder: fmt.Sprintf("[dropped: %s · %s at step %d · recall %s]", target, reason, at, id)}
	}
	return drops, steps
}

// Digest probes never update Workspace's authorization observations.
func (s *Session) gcDigest(abs string) string {
	if s.workspace == nil {
		return ""
	}
	for _, ws := range s.workspace.handles {
		if !insidePath(ws.Root(), abs) {
			continue
		}
		checked, bad := ws.resolve(abs)
		if bad != nil {
			return ""
		}
		// Nonblocking open also covers a regular-file-to-FIFO race.
		file, err := os.OpenFile(checked, os.O_RDONLY|syscall.O_NONBLOCK, 0)
		if os.IsNotExist(err) {
			return "missing"
		}
		if err != nil {
			return ""
		}
		defer file.Close()
		info, err := file.Stat()
		if err != nil || !info.Mode().IsRegular() {
			return ""
		}
		h := sha256.New()
		if _, err := io.Copy(h, file); err != nil {
			return ""
		}
		return fmt.Sprintf("%x", h.Sum(nil))
	}
	return ""
}

func (r *Run) collectContext() bool {
	s := r.session
	if !r.cfg.ContextGC || !s.lastRequest.Known || s.lastRequest.InputTokens < contextGCMinTokens {
		return false
	}
	_, steps, settled := gcResults(s.history, s.gc.Paths)
	if !settled || steps-s.gc.LastPass < contextGCInterval {
		return false
	}
	drops, _ := gcCandidates(s.history, s.gc, s.gcDigest)
	if len(drops) == 0 {
		return false
	}
	before := BuildContext(r.cfg, RequestScope{}, s.requestHistory())
	oldBytes, err := admissionRequestBytes(r.cfg, before)
	if err != nil {
		return false
	}
	merged := make(map[string]contextGCDrop, len(s.gc.Dropped)+len(drops))
	for id, drop := range s.gc.Dropped {
		merged[id] = drop
	}
	for id, drop := range drops {
		merged[id] = drop
	}
	view := gcHistory(s.history, merged)
	// Rebuild a switched projection only at the pass boundary.
	handoff := s.handoff
	if handoff != nil {
		handoff, err = renderModelHandoff(view[:handoff.Entries])
		if err != nil {
			return false
		}
		view = append([]Entry{{Kind: EntryUser, User: &UserTurn{Text: handoff.Text, Plan: handoff.Plan}}}, view[handoff.Entries:]...)
	}
	after := BuildContext(r.cfg, RequestScope{}, view)
	newBytes, err := admissionRequestBytes(r.cfg, after)
	if err != nil {
		return false
	}
	freed := oldBytes - newBytes
	rate := s.tokensPerByte
	if rate <= 0 {
		return false
	}
	if float64(freed)*rate < float64(s.lastRequest.InputTokens)*contextGCFraction {
		return false
	}
	breakEntry := len(s.history)
	ordered := make([]map[string]any, 0, len(drops))
	for i, entry := range s.history {
		if entry.Tool == nil {
			continue
		}
		if drop, ok := drops[entry.Tool.CallID]; ok {
			breakEntry = min(breakEntry, i)
			ordered = append(ordered, map[string]any{"call_id": entry.Tool.CallID, "reason": drop.Reason, "step": drop.Step})
		}
	}
	if s.handoff != nil {
		if breakEntry < s.handoff.Entries {
			breakEntry = 0
		} else {
			breakEntry = breakEntry - s.handoff.Entries + 1
		}
	}
	oldBody, err := encodeRequest(r.cfg, before)
	if err != nil {
		return false
	}
	newBody, err := encodeRequest(r.cfg, after)
	if err != nil {
		return false
	}
	breakByte := 0
	for breakByte < min(len(oldBody), len(newBody)) && oldBody[breakByte] == newBody[breakByte] {
		breakByte++
	}
	s.gc.Dropped, s.gc.LastPass, s.handoff = merged, steps, handoff
	tokens := int64(math.Ceil(float64(freed) * rate))
	r.trace.Write("context.gc", r.steps, map[string]any{"dropped": ordered, "bytes_freed": freed, "estimated_tokens_freed": tokens, "cache_break_entry": breakEntry + 1, "cache_break_byte": breakByte})
	s.display.note(fmt.Sprintf("context GC: dropped %d results, ~%s tokens", len(drops), formatCount(tokens)))
	return true
}

func (s *Session) copyGC() contextGCState {
	state := contextGCState{Dropped: make(map[string]contextGCDrop), Paths: make(map[string]string), Inherited: make(map[string]ToolOutcome)}
	for id, drop := range s.gc.Dropped {
		state.Dropped[id] = drop
	}
	for id, path := range s.gc.Paths {
		state.Paths[id] = path
	}
	for id, out := range s.gc.Inherited {
		state.Inherited[id] = out
	}
	for _, entry := range s.history {
		if entry.Tool != nil {
			if _, ok := state.Dropped[entry.Tool.CallID]; ok {
				state.Inherited[entry.Tool.CallID] = entry.Tool.Outcome
			}
		}
	}
	return state
}
