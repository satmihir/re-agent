package reagent

import (
	"fmt"
	"sort"
	"strings"
)

type agentObservation struct {
	AgentThread
	Depth int `json:"depth"`
}

type agentSubtree struct {
	Threads   []agentObservation `json:"threads"`
	TotalCost Usage              `json:"total_cost"`
}

// Retain accounting, not Sessions, when anonymous or dismissed agents release capacity.
func (t *agentTree) retireLocked(id string) {
	n := t.nodes[id]
	t.archive[id] = agentObservation{AgentThread: n.AgentThread, Depth: n.depth}
	delete(t.nodes, id)
}

func (t *agentTree) subtreeLocked(parent string) agentSubtree {
	records := make(map[string]agentObservation, len(t.archive)+len(t.nodes))
	for id, n := range t.archive {
		records[id] = n
	}
	for id, n := range t.nodes {
		records[id] = agentObservation{AgentThread: n.AgentThread, Depth: n.depth}
	}
	out := agentSubtree{Threads: []agentObservation{}, TotalCost: Usage{Known: true}}
	for _, n := range records {
		ancestor := n.ParentID
		for ancestor != parent && ancestor != "root" && ancestor != "" {
			ancestor = records[ancestor].ParentID
		}
		if ancestor != parent {
			continue
		}
		n.Task = oneLineTask(n.Task)
		out.Threads = append(out.Threads, n)
		out.TotalCost.Add(n.Cost)
	}
	sort.Slice(out.Threads, func(i, j int) bool { return out.Threads[i].ID < out.Threads[j].ID })
	return out
}

func (s *Session) agentSubtree() agentSubtree {
	t := s.agents
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.subtreeLocked(s.agentParent())
}

// All display data is copied under the tree lock; no Session is inspected.
func (t *agentTree) displaySnapshot(parent string, targets []string) (int, string, []string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	running := 0
	for _, n := range t.nodes {
		if n.working {
			running++
		}
	}
	var labels []string
	for _, target := range targets {
		for _, n := range t.childrenLocked(parent) {
			if n.ID == target || n.Name == target {
				name := n.ID
				if n.Name != "" {
					name += " " + n.Name
				}
				labels = append(labels, fmt.Sprintf("%s (step %d)", name, n.Steps))
				break
			}
		}
	}
	notices := t.notices
	t.notices = nil
	return running, "waiting on " + strings.Join(labels, ", "), notices
}

func (d *Display) refreshAgents() {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.refreshAgentsLocked()
}

func (d *Display) refreshAgentsLocked() {
	if d.agents == nil {
		return
	}
	var targets []string
	if d.status != nil {
		targets = d.status.waitIDs
	}
	count, label, notices := d.agents.displaySnapshot("root", targets)
	if len(targets) > 0 {
		d.status.label = label
	}
	if d.input != nil {
		d.input.regionStatus.agents = count
	}
	for _, notice := range notices {
		d.eraseStatusLocked()
		fmt.Fprintln(d.w, "  ↳ "+regionLabel(notice))
	}
}

func (d *Display) agentWaitStarted(ids []string) {
	if d.agents == nil {
		return
	}
	d.startStatus("waiting on " + strings.Join(ids, ", "))
	d.mu.Lock()
	if d.status != nil {
		d.status.waitIDs = append([]string(nil), ids...)
	}
	d.refreshAgentsLocked()
	d.mu.Unlock()
}
