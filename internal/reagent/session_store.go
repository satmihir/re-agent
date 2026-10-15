package reagent

import (
	"bytes"
	"crypto/sha256"
	"encoding/gob"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"syscall"
	"time"
)

// v0 §10 amendment (2026-10-02): checkpoints are execution barriers, not traces.
const checkpointVersion = 2

var sessionIDPattern = regexp.MustCompile(`^[0-9a-f]{16}$`)

type chatCheckpoint struct {
	Version               int             `json:"version"`
	ID                    string          `json:"id"`
	Provider              string          `json:"provider"`
	Model                 string          `json:"model"`
	Effort                string          `json:"effort"`
	Launch                string          `json:"launch"`
	Active                string          `json:"active"`
	ReadOnly              bool            `json:"read_only"`
	Plan                  bool            `json:"plan"`
	NoProjectInstructions bool            `json:"no_project_instructions"`
	ProjectInstructions   *string         `json:"project_instructions,omitempty"`
	LaunchInstructions    *string         `json:"launch_instructions,omitempty"`
	MaxSteps              int             `json:"max_steps"`
	MaxToolCalls          int             `json:"max_tool_calls"`
	ModelRetryWindow      time.Duration   `json:"model_retry_window"`
	InRunCompact          bool            `json:"in_run_compact"`
	ContextGC             bool            `json:"context_gc"`
	GC                    contextGCState  `json:"gc_state"`
	GCRequests            int             `json:"gc_requests,omitempty"`
	Agents                bool            `json:"agents"`
	AgentRoster           []AgentThread   `json:"agent_roster,omitempty"`
	AgentSubtree          *agentSubtree   `json:"agent_subtree,omitempty"`
	ReportFriction        bool            `json:"report_friction"`
	Auto                  bool            `json:"auto"`
	AutoEnabled           bool            `json:"auto_enabled"`
	AutoFallbackModel     string          `json:"auto_fallback_model,omitempty"`
	AutoFallbackEffort    string          `json:"auto_fallback_effort,omitempty"`
	AutoUsage             Usage           `json:"auto_usage"`
	AutoAttempts          int             `json:"auto_attempts"`
	AutoCompactOff        bool            `json:"auto_compact_off"`
	History               []Entry         `json:"history"`
	PendingSubmission     *UserTurn       `json:"pending_submission,omitempty"`
	Seen                  map[string]bool `json:"seen"`
	Handoff               *modelHandoff   `json:"handoff,omitempty"`
	CompactedPlan         string          `json:"compacted_plan"`
	Blocked               string          `json:"blocked"`
	LastTrace             string          `json:"last_trace"`
	LastRequest           Usage           `json:"last_request"`
	TokensPerByte         float64         `json:"tokens_per_byte"`
	Usage                 Usage           `json:"usage"`
	Phase                 string          `json:"phase"`
	InFlight              string          `json:"in_flight,omitempty"`
	Prompt                *pinnedPrompt   `json:"prompt,omitempty"` // absent before instructions were pinned
}

type checkpointEnvelope struct {
	Version int             `json:"version"`
	ID      string          `json:"id"`
	State   json.RawMessage `json:"state"`
	Digest  string          `json:"sha256"`
}

type sessionStore struct {
	dir  string
	lock *os.File
}

func checkpointDirectory(id string) (string, error) {
	if !sessionIDPattern.MatchString(id) {
		return "", fmt.Errorf("invalid session ID %q: expected 16 lowercase hex digits", id)
	}
	root, err := os.UserCacheDir()
	if err != nil {
		return "", fmt.Errorf("user cache directory is unavailable")
	}
	dir := filepath.Join(root, "reagent", "sessions", id)
	if testCachePath != nil {
		testCachePath(dir)
	}
	return dir, nil
}

func privateDirectory(path string, create bool) error {
	if create {
		if err := os.MkdirAll(path, 0o700); err != nil {
			return fmt.Errorf("create session directory: %s", stableError(err))
		}
	}
	info, err := os.Lstat(path)
	if err != nil {
		return fmt.Errorf("inspect session directory: %s", stableError(err))
	}
	if !info.IsDir() || info.Mode().Perm() != 0o700 {
		return fmt.Errorf("session directory %s must be private (0700) and not a symlink", path)
	}
	return nil
}

func openSessionStore(id string, create bool) (*sessionStore, error) {
	dir, err := checkpointDirectory(id)
	if err != nil {
		return nil, err
	}
	parent := filepath.Dir(dir)
	if err := privateDirectory(parent, create); err != nil {
		return nil, fmt.Errorf("open session store: %w", err)
	}
	if create {
		if err := os.Mkdir(dir, 0o700); err != nil {
			return nil, fmt.Errorf("create session %s: %s", id, stableError(err))
		}
	}
	if err := privateDirectory(dir, false); err != nil {
		return nil, fmt.Errorf("open session %s: %w", id, err)
	}
	lock, err := os.OpenFile(filepath.Join(dir, "lock"), os.O_CREATE|os.O_RDWR|syscall.O_NOFOLLOW, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open session lock: %s", stableError(err))
	}
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		lock.Close()
		return nil, fmt.Errorf("session %s is already open or cannot be locked", id)
	}
	return &sessionStore{dir: dir, lock: lock}, nil
}

func (st *sessionStore) close() {
	if st != nil && st.lock != nil {
		syscall.Flock(int(st.lock.Fd()), syscall.LOCK_UN)
		st.lock.Close()
		st.lock = nil
	}
}

func (st *sessionStore) save(cp chatCheckpoint) error {
	if testCachePath != nil {
		testCachePath(st.dir)
		testCachePath(filepath.Join(st.dir, "checkpoint.json"))
	}
	cp.Version = checkpointVersion
	persisted, err := checkpointRawJSON(cp, false)
	if err != nil {
		return err
	}
	state, err := canonicalCheckpointJSON(persisted)
	if err != nil {
		return err
	}
	body, err := canonicalCheckpointJSON(checkpointEnvelope{Version: checkpointVersion, ID: cp.ID, State: state, Digest: fmt.Sprintf("%x", sha256.Sum256(state))})
	if err != nil {
		return err
	}
	file, err := os.CreateTemp(st.dir, ".checkpoint-*")
	if err != nil {
		return fmt.Errorf("stage checkpoint: %s", stableError(err))
	}
	defer os.Remove(file.Name())
	if _, err = file.Write(body); err != nil {
		file.Close()
		return fmt.Errorf("write checkpoint: %s", stableError(err))
	}
	if err = file.Sync(); err != nil {
		file.Close()
		return fmt.Errorf("sync checkpoint: %s", stableError(err))
	}
	if err = file.Close(); err != nil {
		return fmt.Errorf("close checkpoint: %s", stableError(err))
	}
	if err = os.Rename(file.Name(), filepath.Join(st.dir, "checkpoint.json")); err != nil {
		return fmt.Errorf("replace checkpoint: %s", stableError(err))
	}
	dir, err := os.Open(st.dir)
	if err != nil {
		return fmt.Errorf("open checkpoint directory: %s", stableError(err))
	}
	defer dir.Close()
	if err := dir.Sync(); err != nil {
		return fmt.Errorf("sync checkpoint directory: %s", stableError(err))
	}
	return nil
}

func (st *sessionStore) load(id string) (chatCheckpoint, error) {
	if testCachePath != nil {
		testCachePath(st.dir)
		testCachePath(filepath.Join(st.dir, "checkpoint.json"))
	}
	var cp chatCheckpoint
	path := filepath.Join(st.dir, "checkpoint.json")
	info, err := os.Lstat(path)
	if err != nil {
		return cp, fmt.Errorf("session %s checkpoint: %s", id, stableError(err))
	}
	if !info.Mode().IsRegular() || info.Mode().Perm() != 0o600 {
		return cp, fmt.Errorf("session %s checkpoint is not a private regular file", id)
	}
	body, err := os.ReadFile(path)
	if err != nil {
		return cp, fmt.Errorf("read session %s: %s", id, stableError(err))
	}
	var envelope checkpointEnvelope
	if err := json.Unmarshal(body, &envelope); err != nil {
		return cp, fmt.Errorf("corrupt session %s: invalid checkpoint JSON", id)
	}
	if envelope.Version != 1 && envelope.Version != checkpointVersion {
		return cp, fmt.Errorf("session %s has unsupported checkpoint version %d", id, envelope.Version)
	}
	if envelope.ID != id || len(envelope.State) == 0 {
		return cp, fmt.Errorf("corrupt session %s: invalid checkpoint envelope", id)
	}
	state := []byte(envelope.State)
	if envelope.Version == 1 {
		if err := json.Unmarshal(envelope.State, &state); err != nil {
			return cp, fmt.Errorf("corrupt session %s: invalid legacy checkpoint payload", id)
		}
	}
	if envelope.Digest != fmt.Sprintf("%x", sha256.Sum256(state)) {
		return cp, fmt.Errorf("corrupt session %s: checkpoint checksum mismatch", id)
	}
	if envelope.Version == 1 {
		if err := gob.NewDecoder(bytes.NewReader(state)).Decode(&cp); err != nil {
			return cp, fmt.Errorf("corrupt session %s: invalid legacy checkpoint state", id)
		}
	} else {
		if err := json.Unmarshal(state, &cp); err != nil {
			return cp, fmt.Errorf("corrupt session %s: invalid checkpoint state", id)
		}
		cp, err = checkpointRawJSON(cp, true)
		if err != nil {
			return cp, fmt.Errorf("corrupt session %s: %w", id, err)
		}
	}
	if cp.Version != envelope.Version {
		return cp, fmt.Errorf("corrupt session %s: mismatched version", id)
	}
	if cp.ID != id || cp.Launch == "" || cp.Provider == "" || cp.Model == "" || cp.MaxSteps < 0 || cp.MaxToolCalls < 0 || cp.ModelRetryWindow < 0 {
		return cp, fmt.Errorf("corrupt session %s: invalid configuration", id)
	}
	switch cp.Phase {
	case "idle", "model", "accepted", "tool", "terminal":
	default:
		return cp, fmt.Errorf("corrupt session %s: unknown checkpoint phase", id)
	}
	pending := make(map[string]string)
	accepted := make(map[string]bool)
	for i, e := range cp.History {
		n := 0
		for _, present := range []bool{e.User != nil, e.Assistant != nil, e.Tool != nil, e.Shell != nil, e.Summary != nil} {
			if present {
				n++
			}
		}
		if n != 1 || (e.Kind == EntryUser && e.User == nil) || (e.Kind == EntryAssistant && e.Assistant == nil) || (e.Kind == EntryTool && e.Tool == nil) || (e.Kind == EntryShell && e.Shell == nil) || (e.Kind == EntrySummary && e.Summary == nil) {
			return cp, fmt.Errorf("corrupt session %s: entry %d", id, i+1)
		}
		if e.Assistant != nil {
			for _, call := range toolCalls(*e.Assistant) {
				if call == nil || call.CallID == "" || call.Name == "" || accepted[call.CallID] || !cp.Seen[call.CallID] {
					return cp, fmt.Errorf("corrupt session %s: invalid call in entry %d", id, i+1)
				}
				pending[call.CallID] = call.Name
				accepted[call.CallID] = true
			}
		}
		if e.Tool != nil {
			if pending[e.Tool.CallID] == "" || pending[e.Tool.CallID] != e.Tool.Name {
				return cp, fmt.Errorf("corrupt session %s: unmatched result in entry %d", id, i+1)
			}
			delete(pending, e.Tool.CallID)
		}
	}
	if cp.Phase == "tool" {
		if pending[cp.InFlight] == "" {
			return cp, fmt.Errorf("corrupt session %s: missing in-flight call", id)
		}
	} else if cp.InFlight != "" {
		return cp, fmt.Errorf("corrupt session %s: unexpected in-flight call", id)
	}
	if len(pending) != 0 && cp.Phase != "accepted" && cp.Phase != "tool" {
		return cp, fmt.Errorf("corrupt session %s: unresolved calls outside an accepted batch", id)
	}
	if cp.Handoff != nil && (cp.Handoff.Entries < 0 || cp.Handoff.Entries > len(cp.History)) {
		return cp, fmt.Errorf("corrupt session %s: handoff", id)
	}
	return cp, nil
}
