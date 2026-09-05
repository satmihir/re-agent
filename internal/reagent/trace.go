package reagent

import (
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"
)

// traceSchemaVersion is the version of the event envelope, not of the payloads.
const traceSchemaVersion = 1

// event is one line of the trace. Sequence numbers establish order; timestamps
// are diagnostic (v1 §16.2).
type event struct {
	SchemaVersion int    `json:"schema_version"`
	Seq           int    `json:"seq"`
	SessionID     string `json:"session_id"`
	RunID         string `json:"run_id"`
	Time          string `json:"time"`
	Type          string `json:"type"`
	Step          int    `json:"step"`
	Data          any    `json:"data"`
}

// Trace is a best-effort JSONL record of one run.
//
// v0 deliberately drops v1's I13: a failed write warns once, disables further
// writes, and lets the run continue. v1 §16.5 restores fail-closed recording.
type Trace struct {
	file      *os.File
	path      string
	sessionID string
	runID     string
	warn      io.Writer
	seq       int
	off       bool
}

// OpenTrace creates a new trace file. It always returns a usable Trace: if the
// file cannot be created, the returned Trace warns once and records nothing.
func OpenTrace(path, sessionID, runID string, warn io.Writer) *Trace {
	t := &Trace{path: path, sessionID: sessionID, runID: runID, warn: warn}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.disable(err)
		return t
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		t.disable(err)
		return t
	}
	t.file = f
	return t
}

// DefaultTracePath is where a run records itself when no path is given.
func DefaultTracePath(runID string) (string, error) {
	dir, err := os.UserCacheDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "reagent", "runs", runID, "events.jsonl"), nil
}

// Write appends one event. Callers do not check for failure: recording is best
// effort in v0, and a disabled trace is reported once, on stderr.
func (t *Trace) Write(kind string, step int, data any) {
	if t.off {
		return
	}
	t.seq++
	line, err := json.Marshal(event{
		SchemaVersion: traceSchemaVersion,
		Seq:           t.seq,
		SessionID:     t.sessionID,
		RunID:         t.runID,
		Time:          time.Now().UTC().Format(time.RFC3339Nano),
		Type:          kind,
		Step:          step,
		Data:          data,
	})
	if err != nil {
		t.disable(err)
		return
	}
	if _, err := t.file.Write(append(line, '\n')); err != nil {
		t.disable(err)
	}
}

// Path is the trace file, or empty when nothing was recorded.
func (t *Trace) Path() string {
	if t.off {
		return ""
	}
	return t.path
}

func (t *Trace) Close() {
	if t.file != nil {
		t.file.Close()
	}
}

func (t *Trace) disable(cause error) {
	t.off = true
	if t.file != nil {
		t.file.Close()
		t.file = nil
	}
	fmt.Fprintf(t.warn, "warning: tracing disabled, this run is not recorded: %v\n", cause)
}
