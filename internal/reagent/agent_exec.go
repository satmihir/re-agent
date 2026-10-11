package reagent

import (
	"encoding/json"
	"sort"
)

type agentExec struct {
	AgentID   string   `json:"agent_id"`
	Argv      []string `json:"argv"`
	Cwd       string   `json:"cwd"`
	Truncated bool     `json:"truncated,omitempty"`
}

type activeAgentExec struct {
	agentExec
	workspace string
}

func (t *agentTree) startExec(id, workspace string, args execArgs) []agentExec {
	t.mu.Lock()
	defer t.mu.Unlock()
	var concurrent []agentExec
	for other, running := range t.execs {
		if other != id && (insidePath(running.workspace, workspace) || insidePath(workspace, running.workspace)) {
			concurrent = append(concurrent, running.agentExec)
		}
	}
	sort.Slice(concurrent, func(i, j int) bool { return concurrent[i].AgentID < concurrent[j].AgentID })
	if t.execs == nil {
		t.execs = make(map[string]activeAgentExec)
	}
	t.execs[id] = activeAgentExec{agentExec: agentExec{AgentID: id, Argv: args.Argv, Cwd: args.Cwd}, workspace: workspace}
	return concurrent
}

func (t *agentTree) endExec(id string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	delete(t.execs, id)
}

func boundConcurrentExec(notices []agentExec, budget int) []agentExec {
	if len(notices) == 0 {
		return nil
	}
	limit := min(1024, budget/(2*len(notices)))
	for i := range notices {
		notice := &notices[i]
		// Limit selection before encoding so huge argv cannot delay another agent.
		argv := notice.Argv
		notice.Truncated = len(argv) > 8 || len(notice.Cwd) > limit
		notice.Cwd = truncateUTF8(notice.Cwd, limit)
		notice.Argv = make([]string, 0, min(len(argv), 8))
		for _, argument := range argv[:min(len(argv), 8)] {
			notice.Truncated = notice.Truncated || len(argument) > limit
			notice.Argv = append(notice.Argv, truncateUTF8(argument, limit))
		}
		for {
			raw, _ := json.Marshal(notice)
			if len(raw) <= limit {
				break
			}
			notice.Truncated = true
			if len(notice.Argv) > 1 {
				notice.Argv = notice.Argv[:max(1, len(notice.Argv)/2)]
				continue
			}
			if len(notice.Cwd) > len(notice.Argv[0]) {
				notice.Cwd = truncateUTF8(notice.Cwd, len(notice.Cwd)/2)
				continue
			}
			notice.Argv[0] = truncateUTF8(notice.Argv[0], len(notice.Argv[0])/2)
		}
	}
	return notices
}
