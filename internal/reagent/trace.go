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

// Trace is a best-effort JSONL record of one run at a time.
//
// A session points one recorder at a fresh file for each run, so the loop and
// the adapter, which both hold it, follow the conversation without being
// rebuilt. v0 deliberately drops v1's I13: a failed write warns once, disables
// recording for that run, and lets the run continue (v1 §16.5 restores
// fail-closed recording).
type Trace struct {
	warn      io.Writer
	sessionID string
	runID     string
	path      string
	file      *os.File
	seq       int
	off       bool
}

// NewTrace makes a recorder with nothing open. Nothing is recorded until Open.
func NewTrace(warn io.Writer) *Trace {
	return &Trace{warn: warn}
}

// OpenTrace makes a recorder and opens its first run: the one-shot form.
func OpenTrace(path, sessionID, runID string, warn io.Writer) *Trace {
	t := NewTrace(warn)
	t.Open(sessionID, runID, path)
	return t
}

// Open closes any current file and starts recording a new run into path. A
// failure to create the file is reported once and recording for this run is
// skipped; the next Open tries again.
func (t *Trace) Open(sessionID, runID, path string) {
	t.Close()
	t.sessionID, t.runID, t.path = sessionID, runID, path
	t.seq, t.off = 0, false

	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.disable(err)
		return
	}
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		t.disable(err)
		return
	}
	t.file = file
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
	if t.off || t.file == nil {
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

// Path is the current run's trace file, or empty when it is not being recorded.
func (t *Trace) Path() string {
	if t.off {
		return ""
	}
	return t.path
}

// Close finishes the current run's file, if one is open.
func (t *Trace) Close() {
	if t.file != nil {
		t.file.Close()
		t.file = nil
	}
}

func (t *Trace) disable(cause error) {
	t.off = true
	t.Close()
	fmt.Fprintf(t.warn, "warning: tracing disabled, this run is not recorded: %v\n", cause)
}
