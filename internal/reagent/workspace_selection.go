package reagent

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// v0 §4 amendment (2026-09-30): approval and selection are separate state.
type workspaceDestination struct {
	path      string
	spelling  string
	requested string
	parent    os.FileInfo
	missing   bool
}

type workspaceSelection struct {
	launch   string
	active   *Workspace
	approved map[string]workspaceDestination
	handles  map[string]*Workspace
}

func (s *Session) approvedWorkspacePaths() []string {
	if s.workspace == nil {
		return nil
	}
	paths := make([]string, 0, len(s.workspace.approved))
	for path := range s.workspace.approved {
		paths = append(paths, path)
	}
	sort.Strings(paths)
	return paths
}

func newWorkspaceSelection(ws *Workspace, approved []workspaceDestination) *workspaceSelection {
	active := &Workspace{root: ws.root, digests: make(map[string]map[string]bool)}
	selection := &workspaceSelection{
		launch: ws.Root(), active: active, approved: make(map[string]workspaceDestination),
		handles: map[string]*Workspace{active.root: active},
	}
	selection.approved[active.root] = workspaceDestination{path: active.root}
	for _, destination := range approved {
		selection.approved[destination.path] = destination
	}
	return selection
}

// Model replacement keeps grants and provenance, but not a mutable selection pointer.
func (w *workspaceSelection) copy() *workspaceSelection {
	copy := &workspaceSelection{launch: w.launch, active: w.active, approved: make(map[string]workspaceDestination), handles: make(map[string]*Workspace)}
	for path, destination := range w.approved {
		copy.approved[path] = destination
	}
	for path, handle := range w.handles {
		copy.handles[path] = handle
	}
	return copy
}

func workspaceDestinationAt(ctx context.Context, path string, prospective bool) (workspaceDestination, *ToolOutcome) {
	var destination workspaceDestination
	if strings.TrimSpace(path) == "" || !filepath.IsAbs(path) || strings.ContainsRune(path, 0) {
		return destination, failPtr("invalid_path", "workspace path must be a nonblank absolute path without NUL bytes")
	}
	for _, part := range strings.Split(filepath.ToSlash(path), "/") {
		if part == ".." || withheld(part) {
			return destination, failPtr("invalid_path", "workspace path must not contain parent traversal or withheld names")
		}
	}
	path = filepath.Clean(path)
	destination.requested = path
	if filepath.Dir(path) == path {
		return destination, failPtr("invalid_path", "the filesystem root cannot be selected")
	}
	parent, err := filepath.EvalSymlinks(filepath.Dir(path))
	if err != nil {
		return destination, osOutcome(err)
	}
	parentInfo, err := os.Stat(parent)
	if err != nil {
		return destination, osOutcome(err)
	}
	if !parentInfo.IsDir() {
		return destination, failPtr("not_directory", "workspace parent is not a directory")
	}
	destination.spelling = filepath.Join(parent, filepath.Base(path))
	destination.parent = parentInfo
	info, err := os.Lstat(destination.spelling)
	if os.IsNotExist(err) && prospective {
		destination.path, destination.missing = destination.spelling, true
	} else {
		if err != nil {
			return destination, osOutcome(err)
		}
		root, err := filepath.EvalSymlinks(destination.spelling)
		if err != nil {
			return destination, osOutcome(err)
		}
		if info.Mode()&os.ModeSymlink != 0 {
			info, err = os.Stat(root)
			if err != nil {
				return destination, osOutcome(err)
			}
		}
		if !info.IsDir() {
			return destination, failPtr("not_directory", "workspace is not a directory")
		}
		destination.path = root
	}
	if filepath.Dir(destination.path) == destination.path || withheldPath(destination.path) {
		return destination, failPtr("invalid_path", "resolved workspace is a filesystem root or withheld location")
	}
	if !destination.missing {
		if bad := validateLinkedWorktree(ctx, destination.path); bad != nil {
			return destination, bad
		}
	}
	return destination, nil
}

func (w *workspaceSelection) permitted(destination workspaceDestination) (bool, *ToolOutcome) {
	permitted := false
	for root, grant := range w.approved {
		if !insidePath(root, destination.path) && !insidePath(root, destination.spelling) && (grant.requested == "" || !insidePath(grant.requested, destination.requested)) {
			continue
		}
		if grant.missing {
			parent, err := os.Stat(filepath.Dir(root))
			if err != nil || !os.SameFile(parent, grant.parent) {
				return false, failPtr("workspace_validation_failed", "the approved destination's parent changed; restart to approve the changed location")
			}
			canonicalParent, err := filepath.EvalSymlinks(filepath.Dir(root))
			if err != nil || canonicalParent != filepath.Dir(root) {
				return false, failPtr("workspace_validation_failed", "the approved destination's parent was redirected")
			}
			if info, err := os.Lstat(root); err == nil && info.Mode()&os.ModeSymlink != 0 {
				return false, failPtr("workspace_validation_failed", "the approved destination became a symlink")
			}
		}
		if insidePath(root, destination.path) {
			permitted = true
		}
	}
	return permitted, nil
}

func (s *Session) approveWorkspace(ctx context.Context, destination workspaceDestination) *ToolOutcome {
	if ctx.Err() != nil {
		return failPtr("permission_denied", "workspace request cancelled")
	}
	permitted, bad := s.workspace.permitted(destination)
	if bad != nil {
		return bad
	}
	if permitted {
		return nil
	}
	if s.workspaceConsent == nil {
		return failPtr("permission_required", "workspace access needs human consent; use interactive chat or --allow-workspace PATH")
	}
	s.display.stopStatus()
	allowed, err := s.workspaceConsent(ctx, destination)
	if err != nil {
		if err == errNotInteractive {
			return failPtr("permission_required", "workspace consent needs a terminal; preapprove the exact path with --allow-workspace PATH")
		}
		return failPtr("permission_denied", "workspace consent was cancelled or unavailable")
	}
	if !allowed || ctx.Err() != nil {
		return failPtr("permission_denied", "workspace access was not approved")
	}
	return nil
}

// v0 §4 amendment (2026-09-30): Git identity validates a linked worktree, not authority.
func validateLinkedWorktree(ctx context.Context, root string) *ToolOutcome {
	rootInfo, err := os.Stat(root)
	if err != nil || !rootInfo.IsDir() {
		return failPtr("workspace_validation_failed", "cannot identify selected worktree directory")
	}
	info, err := os.Lstat(filepath.Join(root, ".git"))
	if os.IsNotExist(err) || (err == nil && info.IsDir()) {
		return nil
	}
	if err != nil || !info.Mode().IsRegular() {
		return failPtr("workspace_validation_failed", "cannot validate workspace Git metadata")
	}
	top, ok := snapshotGit(ctx, root, "rev-parse", "--show-toplevel")
	if !ok {
		return failPtr("workspace_validation_failed", "cannot identify linked worktree")
	}
	canonicalTop, err := filepath.EvalSymlinks(strings.TrimSpace(top))
	topInfo, statErr := os.Stat(canonicalTop)
	// v0 §4 amendment (2026-09-30): Git may report the on-disk case, not the launch spelling.
	if err != nil || statErr != nil || !os.SameFile(rootInfo, topInfo) {
		return failPtr("workspace_validation_failed", "linked worktree top-level does not match the selected directory")
	}
	common, ok := snapshotGit(ctx, root, "rev-parse", "--git-common-dir")
	if !ok {
		return failPtr("workspace_validation_failed", "cannot identify linked worktree common directory")
	}
	common = strings.TrimSpace(common)
	if !filepath.IsAbs(common) {
		common = filepath.Join(root, common)
	}
	common, err = filepath.EvalSymlinks(common)
	if err != nil {
		return failPtr("workspace_validation_failed", "cannot resolve linked worktree common directory")
	}
	if info, err := os.Stat(common); err != nil || !info.IsDir() {
		return failPtr("workspace_validation_failed", "linked worktree common directory is not a directory")
	}
	listing, ok := snapshotGit(ctx, root, "worktree", "list", "--porcelain", "-z")
	if !ok {
		return failPtr("workspace_validation_failed", "cannot read a complete repository worktree list")
	}
	for _, field := range strings.Split(listing, "\x00") {
		if strings.HasPrefix(field, "worktree ") {
			listedInfo, err := os.Stat(strings.TrimPrefix(field, "worktree "))
			if err == nil && os.SameFile(rootInfo, listedInfo) {
				return nil
			}
		}
	}
	return failPtr("workspace_validation_failed", "selected directory is not in the repository's worktree list")
}

func (s *Session) selectWorkspace(ctx context.Context, destination workspaceDestination) (ToolOutcome, error) {
	previous := s.workspace.active.Root()
	if bad := s.approveWorkspace(ctx, destination); bad != nil {
		return *bad, nil
	}
	if previous == destination.path {
		return workspaceOutcome(workspaceSwitchResult{Previous: previous, Workspace: previous}, previous)
	}
	// Revalidate after waiting for the human, before loading or publishing anything.
	current, bad := workspaceDestinationAt(ctx, destination.spelling, false)
	if bad != nil {
		return *bad, nil
	}
	if current.path != destination.path || !os.SameFile(current.parent, destination.parent) || ctx.Err() != nil {
		return failOutcome("workspace_validation_failed", "workspace changed or request was cancelled during consent"), nil
	}
	if _, bad := s.workspace.permitted(current); bad != nil {
		return *bad, nil
	}
	ws := s.workspace.handles[current.path]
	if ws == nil {
		ws = &Workspace{root: current.path, digests: make(map[string]map[string]bool)}
	}
	var warnings bytes.Buffer
	project := loadProjectInstructions(ws, s.cfg.NoProjectInstructions, &warnings)
	state := collectSnapshot(ctx, ws.Root(), s.refsOnlySnapshot(ws.Root()))
	if ctx.Err() != nil {
		return failOutcome("permission_denied", "workspace request cancelled"), nil
	}
	result := workspaceSwitchResult{Previous: previous, Workspace: ws.Root(), Changed: true, State: state, Warnings: warnings.String()}
	outcome, err := workspaceOutcome(result, previous)
	if err != nil {
		return ToolOutcome{}, err
	}
	if encodedSize(outcome) > MaxResultBytes {
		return failOutcome("invalid_arguments", "workspace transition metadata exceeds the result limit"), nil
	}
	// v0 §6 amendment (2026-09-30): one dispatch boundary publishes all consumers.
	s.workspace.active = ws
	s.workspace.handles[ws.Root()] = ws
	if _, exists := s.workspace.approved[ws.Root()]; !exists {
		s.workspace.approved[ws.Root()] = destination
	}
	s.cfg.Workspace, s.cfg.WorkspacePath, s.cfg.ProjectInstructions = ws, ws.Root(), project
	s.cfg.Registry = s.cfg.Registry.bindWorkspace(s, ws)
	if !s.planMode && !s.cfg.Registry.Mode().ReadOnly {
		s.display.headerLine("! exec mode: commands run as you, in " + sanitize(shortPath(ws.Root())) + ", and can read, write, and use the network")
	}
	if warnings.Len() != 0 {
		fmt.Fprint(s.progress, warnings.String())
	}
	return outcome, nil
}
