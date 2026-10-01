package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// workspaceState is the dated, best-effort record attached to one user turn.
// v0 §6 amendment (2026-09-27): collect before the run, not in BuildContext.
type workspaceState struct {
	Kind      string    `json:"kind"`
	Workspace string    `json:"workspace"`
	Date      string    `json:"date,omitempty"`
	TimeZone  string    `json:"time_zone,omitempty"`
	Git       *gitState `json:"git,omitempty"`
}

type gitState struct {
	Branch         string `json:"branch,omitempty"`
	Upstream       string `json:"upstream,omitempty"`
	Ahead          *int   `json:"ahead,omitempty"`
	Behind         *int   `json:"behind,omitempty"`
	DefaultBranch  string `json:"default_branch,omitempty"`
	AheadOfDefault *int   `json:"ahead_of_default,omitempty"`
	BehindDefault  *int   `json:"behind_default,omitempty"`
	LastFetch      string `json:"last_fetch,omitempty"`
	Staged         int    `json:"staged"`
	Modified       int    `json:"modified"`
	Untracked      int    `json:"untracked"`
}

const workspacePreamble = "Workspace state when this message was sent, collected by re:agent:\n"

// collectSnapshot omits fields that cannot be observed; it never fetches or guesses a ref.
func collectSnapshot(ctx context.Context, root string) json.RawMessage {
	now := time.Now()
	zone, _ := now.Zone()
	state := workspaceState{Kind: "workspace_state", Workspace: root, Date: now.Format("2006-01-02"), TimeZone: zone}
	if status, ok := snapshotGit(ctx, root, "status", "--porcelain=v2", "--branch"); ok {
		git := parseGitStatus(status)
		// v0 §6 U4 review: a fallback ref is a candidate, not an observation.
		defaultBranch := "origin/main"
		if name, ok := snapshotGit(ctx, root, "symbolic-ref", "--short", "refs/remotes/origin/HEAD"); ok && strings.TrimSpace(name) != "" {
			defaultBranch = strings.TrimSpace(name)
		}
		if counts, ok := snapshotGit(ctx, root, "rev-list", "--left-right", "--count", "HEAD..."+defaultBranch); ok {
			ahead, behind := parseCounts(counts)
			if ahead != nil && behind != nil {
				git.DefaultBranch = defaultBranch
				git.AheadOfDefault, git.BehindDefault = ahead, behind
			}
		}
		if path, ok := snapshotGit(ctx, root, "rev-parse", "--git-path", "FETCH_HEAD"); ok {
			fetch := strings.TrimSpace(path)
			if !filepath.IsAbs(fetch) {
				fetch = filepath.Join(root, fetch)
			}
			if info, err := os.Stat(fetch); err == nil && info.Mode().IsRegular() {
				git.LastFetch = info.ModTime().In(time.Local).Format(time.RFC3339)
			}
		}
		state.Git = &git
	}
	raw, _ := json.Marshal(state)
	return raw
}

func snapshotGit(ctx context.Context, root string, args ...string) (string, bool) {
	deadline, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()
	// v0 §6 amendment (2026-09-30): metadata probes must not run configured fsmonitor commands.
	cmd := exec.CommandContext(deadline, "git", append([]string{"-c", "core.fsmonitor=false"}, args...)...)
	cmd.Dir = root
	cmd.Env = childEnvironment()
	cmd.Stdin = bytes.NewReader(nil)
	cmd.WaitDelay = time.Second
	out := &boundedWriter{limit: MaxResultBytes}
	cmd.Stdout = out
	err := cmd.Run()
	return string(out.kept), err == nil && out.seen <= MaxResultBytes
}

func parseGitStatus(status string) gitState {
	var state gitState
	for _, line := range strings.Split(status, "\n") {
		switch {
		case strings.HasPrefix(line, "# branch.head "):
			state.Branch = strings.TrimPrefix(line, "# branch.head ")
			if state.Branch == "(unknown)" {
				state.Branch = "(detached)"
			}
		case strings.HasPrefix(line, "# branch.upstream "):
			state.Upstream = strings.TrimPrefix(line, "# branch.upstream ")
		case strings.HasPrefix(line, "# branch.ab "):
			fields := strings.Fields(strings.TrimPrefix(line, "# branch.ab "))
			if len(fields) == 2 && strings.HasPrefix(fields[0], "+") && strings.HasPrefix(fields[1], "-") {
				state.Ahead, state.Behind = parseCounts(fields[0][1:] + " " + fields[1][1:])
			}
		case strings.HasPrefix(line, "? "):
			state.Untracked++
		case strings.HasPrefix(line, "1 "), strings.HasPrefix(line, "2 "), strings.HasPrefix(line, "u "):
			fields := strings.Fields(line)
			if len(fields) > 1 && len(fields[1]) == 2 {
				if fields[1][0] != '.' {
					state.Staged++
				}
				if fields[1][1] != '.' {
					state.Modified++
				}
			}
		}
	}
	return state
}

func parseCounts(output string) (*int, *int) {
	fields := strings.Fields(output)
	if len(fields) != 2 {
		return nil, nil
	}
	left, err := strconv.Atoi(fields[0])
	if err != nil || left < 0 {
		return nil, nil
	}
	right, err := strconv.Atoi(fields[1])
	if err != nil || right < 0 {
		return nil, nil
	}
	return &left, &right
}
