package reagent

import (
	"context"
	"fmt"
	"io"
)

// Session is what outlives one run: the fixed configuration, the accepted
// transcript, and every call id ever accepted (I04). It keeps one provider,
// one model, and one mode from its first run to its last; /reset makes a new
// one rather than changing any of them (v1 §6.1).
type Session struct {
	ID       string
	cfg      Config
	model    Model
	trace    *Trace
	progress io.Writer

	history   []Entry
	seenCalls map[string]bool

	// blocked explains why ordinary input is refused until Reset. A run whose
	// outcome cannot be continued from sets it; the transcript is never rolled
	// back to hide that outcome (v1 §7.5).
	blocked   string
	lastTrace string
}

// NewSession starts a session with an empty transcript.
func NewSession(cfg Config, model Model, trace *Trace, progress io.Writer) *Session {
	return &Session{
		ID: NewID(), cfg: cfg, model: model, trace: trace, progress: progress,
		seenCalls: make(map[string]bool),
	}
}

// Turn runs one user submission as a new run and returns its result. The run
// appends to this session's transcript, so a later turn sees everything an
// earlier one read or did.
func (s *Session) Turn(ctx context.Context, text, runID, tracePath string) (RunResult, error) {
	if s.blocked != "" {
		return RunResult{}, fmt.Errorf("the last run ended with %s; inspect %s and use /reset to continue",
			s.blocked, s.describeLastTrace())
	}
	s.trace.Open(s.ID, runID, tracePath)
	defer s.trace.Close()

	result := newRun(s, runID).Execute(ctx, text)
	s.lastTrace = result.TracePath
	if !continuable(result.Status) {
		s.blocked = string(result.Status)
	}
	return result, nil
}

// Reset discards the transcript and becomes a fresh session with the same
// launch configuration. Nothing is rolled back: the old history is simply no
// longer sent, and its traces stay on disk.
func (s *Session) Reset() {
	s.ID = NewID()
	s.history = nil
	s.seenCalls = make(map[string]bool)
	s.blocked = ""
	s.lastTrace = ""
}

// LastTrace is the path of the most recent run's trace, or empty.
func (s *Session) LastTrace() string { return s.lastTrace }

func (s *Session) describeLastTrace() string {
	if s.lastTrace == "" {
		return "the run's output above"
	}
	return "the trace at " + s.lastTrace
}

// continuable says which outcomes a session may go on from (v1 §7.5). The
// others leave the transcript ending without an assistant reply, or in a state
// the budget or the provider already refused to continue.
func continuable(status RunStatus) bool {
	return status == StatusCompleted || status == StatusRefused
}
