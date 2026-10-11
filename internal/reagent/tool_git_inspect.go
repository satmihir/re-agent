package reagent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"regexp"
	"strings"
	"time"
	"unicode/utf8"
)

var (
	gitInspectRevision = regexp.MustCompile(`^[A-Za-z0-9_@][A-Za-z0-9_./@{}^~+-]*$`)
	gitInspectOID      = regexp.MustCompile(`^([0-9a-f]{40}|[0-9a-f]{64})$`)
)

type gitInspectTool struct{ ws *Workspace }

// NewGitInspectTool returns fixed read-only operations over local Git history.
func NewGitInspectTool(ws *Workspace) Tool { return gitInspectTool{ws: ws} }

func (t gitInspectTool) withWorkspace(ws *Workspace) Tool { t.ws = ws; return t }

type gitInspectArgs struct {
	Operation string          `json:"operation"`
	From      string          `json:"from"`
	To        string          `json:"to"`
	Rev       string          `json:"rev"`
	Path      string          `json:"path"`
	Paths     []string        `json:"paths"`
	Count     json.RawMessage `json:"count"`
	Stat      json.RawMessage `json:"stat"`
}

type gitInspectResult struct {
	Operation        string `json:"operation"`
	From             string `json:"from,omitempty"`
	To               string `json:"to,omitempty"`
	Rev              string `json:"rev,omitempty"`
	Path             string `json:"path,omitempty"`
	CountLimit       int    `json:"count_limit,omitempty"`
	Output           string `json:"output"`
	OutputBytesSeen  int    `json:"output_bytes_seen"`
	OutputTruncated  bool   `json:"output_truncated"`
	EncodingReplaced bool   `json:"encoding_replaced"`
}

func (gitInspectTool) Spec() ToolSpec {
	return ToolSpec{Name: "git_inspect", Description: "Review local Git changes and history without writes or network, in any mode. diff requires from and to commits, optional literal paths and stat:true for a summary. Working-tree diff is unsupported because it can run clean filters. show uses rev (default HEAD): with path returns a raw historical file; without path returns commit message and stat. log uses optional from and to (default HEAD), meaning from..to when from is given, optional paths, and count (default20, maximum100). Revisions must name single commits, not ranges/options. Paths are workspace-relative; .git/.env names and descendants are excluded even from unscoped file output. Results are a bounded prefix with explicit truncation. Promisor/partial-clone repositories are refused to prevent lazy fetching.", InputSchema: json.RawMessage(`{"type":"object","properties":{"operation":{"type":"string","enum":["diff","show","log"]},"from":{"type":"string"},"to":{"type":"string"},"rev":{"type":"string"},"path":{"type":"string"},"paths":{"type":"array","items":{"type":"string"},"maxItems":32},"count":{"type":"integer","minimum":1,"maximum":100},"stat":{"type":"boolean"}},"required":["operation"],"additionalProperties":false}`), Effect: EffectClassRead}
}

func (t gitInspectTool) Execute(ctx context.Context, args json.RawMessage) (outcome ToolOutcome, err error) {
	defer func() {
		outcome.Effect = EffectNone
		if t.ws != nil {
			outcome.Workspace = t.ws.Root()
		}
	}()
	if t.ws == nil {
		return failOutcome("invalid_workspace", "git_inspect needs a workspace"), nil
	}
	if len(args) > MaxResultBytes || !utf8.Valid(args) {
		return failOutcome("invalid_arguments", "arguments exceed the byte limit or are not UTF-8"), nil
	}
	var a gitInspectArgs
	if bad := decodeArgs(args, &a); bad != nil {
		// Decoder errors can echo an oversized field name into the result.
		return failOutcome("invalid_arguments", "arguments must match the git_inspect schema"), nil
	}
	var fields map[string]json.RawMessage
	json.Unmarshal(args, &fields)
	for _, raw := range fields {
		if strings.TrimSpace(string(raw)) == "null" {
			return failOutcome("invalid_arguments", "omit optional fields rather than using null"), nil
		}
	}
	switch a.Operation {
	case "diff":
		if a.From == "" || a.To == "" {
			return failOutcome("invalid_arguments", "diff requires from and to; working-tree diff is unsupported because it can run clean filters"), nil
		}
		if a.Rev != "" || a.Path != "" || len(a.Count) > 0 {
			return failOutcome("invalid_arguments", "diff accepts only from, to, paths and stat"), nil
		}
	case "show":
		if a.From != "" || a.To != "" || a.Paths != nil || len(a.Count) > 0 || len(a.Stat) > 0 {
			return failOutcome("invalid_arguments", "show accepts only rev and path"), nil
		}
		if a.Rev == "" {
			a.Rev = "HEAD"
		}
	case "log":
		if a.Rev != "" || a.Path != "" || len(a.Stat) > 0 {
			return failOutcome("invalid_arguments", "log accepts only from, to, paths and count"), nil
		}
		if a.To == "" {
			a.To = "HEAD"
		}
	default:
		return failOutcome("invalid_arguments", "operation must be diff, show or log"), nil
	}
	for _, rev := range []string{a.From, a.To, a.Rev} {
		if rev != "" && (len(rev) > 256 || !gitInspectRevision.MatchString(rev) || strings.Contains(rev, "..")) {
			return failOutcome("invalid_revision", "use a single commit name, not an option, path or range"), nil
		}
	}
	if len(a.Paths) > 32 {
		return failOutcome("invalid_arguments", "paths holds at most32 entries"), nil
	}
	for _, p := range a.Paths {
		if !gitInspectPath(p) {
			return failOutcome("invalid_path", "paths must be workspace-relative without parent or withheld components"), nil
		}
	}
	if a.Path != "" {
		if !gitInspectPath(a.Path) || path.Clean(a.Path) == "." {
			return failOutcome("invalid_path", "path must name a readable workspace-relative historical file"), nil
		}
		a.Path = path.Clean(a.Path)
	}
	count, bad := optionalInt(a.Count, "count", 20, 1)
	if bad != nil {
		return *bad, nil
	}
	if count > 100 {
		return failOutcome("invalid_arguments", "count must be at most100"), nil
	}
	stat, bad := optionalBool(a.Stat, "stat")
	if bad != nil {
		return *bad, nil
	}
	if withheldPath(t.ws.Root()) {
		return failOutcome("invalid_path", "workspace must not be under a withheld name"), nil
	}
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	root := t.ws.Root()
	raw, seen, runErr := gitInspectCommand(ctx, root, "rev-parse", "--show-toplevel")
	if runErr != nil || seen > MaxResultBytes {
		return gitInspectFailure(ctx, runErr, "not_repository", "workspace is not inside an available Git working tree"), nil
	}
	top, topErr := filepath.EvalSymlinks(strings.TrimSpace(string(raw)))
	if topErr != nil || !insidePath(top, root) {
		return failOutcome("not_repository", "workspace is not inside the repository working tree"), nil
	}
	// Object reads in partial clones can launch a lazy fetch, including on older Git.
	raw, _, runErr = gitInspectCommand(ctx, root, "config", "--get-regexp", `^(remote\..*\.promisor|extensions\.partialclone)$`)
	if runErr != nil {
		var exit *exec.ExitError
		if !errors.As(runErr, &exit) || exit.ExitCode() != 1 {
			return gitInspectFailure(ctx, runErr, "git_error", "cannot inspect repository configuration"), nil
		}
	}
	if len(raw) > 0 {
		return failOutcome("unsupported_repository", "promisor and partial-clone repositories are refused to prevent lazy fetching"), nil
	}
	resolve := func(rev string) (string, *ToolOutcome) {
		if rev == "" {
			return "", nil
		}
		data, bytesSeen, resolveErr := gitInspectCommand(ctx, root, "rev-parse", "--verify", "--end-of-options", rev+"^{commit}")
		oid := strings.TrimSpace(string(data))
		if resolveErr != nil || bytesSeen > MaxResultBytes || !gitInspectOID.MatchString(oid) {
			bad := gitInspectFailure(ctx, resolveErr, "invalid_revision", "revision does not resolve to one available commit")
			return "", &bad
		}
		return oid, nil
	}
	r := gitInspectResult{Operation: a.Operation, Path: a.Path}
	for _, item := range []struct {
		rev string
		dst *string
	}{{a.From, &r.From}, {a.To, &r.To}, {a.Rev, &r.Rev}} {
		oid, bad := resolve(item.rev)
		if bad != nil {
			return *bad, nil
		}
		*item.dst = oid
	}
	diffOptions := []string{"--no-ext-diff", "--no-textconv", "--no-renames", "--no-color", "--ignore-submodules=none", "--submodule=short", "--relative"}
	var argv []string
	switch a.Operation {
	case "diff":
		argv = append([]string{"diff"}, diffOptions...)
		if stat {
			argv = append(argv, "--stat")
		}
		argv = append(argv, r.From, r.To, "--")
		argv = append(argv, gitInspectPaths(a.Paths)...)
	case "show":
		if a.Path != "" {
			argv = []string{"cat-file", "blob", r.Rev + ":./" + a.Path}
			break
		}
		argv = []string{"show", "--format=fuller", "--stat", "--root", "--no-notes", "--no-show-signature", "--no-use-mailmap"}
		argv = append(argv, diffOptions...)
		argv = append(argv, r.Rev, "--")
		argv = append(argv, gitInspectPaths(nil)...)
	case "log":
		r.CountLimit = count
		rev := r.To
		if r.From != "" {
			rev = r.From + ".." + r.To
		}
		argv = []string{"log", "--format=%H %s", "--no-notes", "--no-show-signature", fmt.Sprintf("--max-count=%d", count), rev, "--"}
		argv = append(argv, gitInspectPaths(a.Paths)...)
	}
	raw, seen, runErr = gitInspectCommand(ctx, root, argv...)
	if runErr != nil {
		return gitInspectFailure(ctx, runErr, "git_error", "Git inspection failed; the object or historical file may be unavailable"), nil
	}
	text := strings.ToValidUTF8(string(raw), "�")
	r.OutputBytesSeen = seen
	r.EncodingReplaced = !utf8.Valid(raw)
	build := func(n int) any {
		result := r
		result.Output = truncateUTF8(text, n)
		result.OutputTruncated = seen > len(raw) || n < len(text)
		return result
	}
	shown := fitElements(len(text), build, root)
	outcome, err = workspaceOutcome(build(shown), root)
	outcome.Truncated = seen > len(raw) || shown < len(text)
	return outcome, err
}

func gitInspectPath(p string) bool {
	if p == "" || len(p) > 4096 || filepath.IsAbs(p) || strings.ContainsAny(p, "\\\x00") {
		return false
	}
	for _, component := range strings.Split(p, "/") {
		if component == ".." || withheld(component) {
			return false
		}
	}
	return true
}

func gitInspectPaths(paths []string) []string {
	result := []string{"."}
	if len(paths) > 0 {
		result = nil
		for _, p := range paths {
			if path.Clean(p) == "." {
				result = append(result, ".")
				continue
			}
			result = append(result, ":(literal)"+path.Clean(p))
		}
	}
	for _, name := range []string{".git", ".env", ".env.*"} {
		result = append(result, ":(exclude,glob,icase)**/"+name, ":(exclude,glob,icase)**/"+name+"/**")
	}
	return result
}

type gitInspectOutput struct {
	prefix []byte
	seen   int
}

func (w *gitInspectOutput) Write(p []byte) (int, error) {
	n := len(p)
	w.seen += n
	keep := min(n, MaxResultBytes-len(w.prefix))
	w.prefix = append(w.prefix, p[:keep]...)
	return n, nil
}

func gitInspectCommand(ctx context.Context, root string, args ...string) ([]byte, int, error) {
	fixed := []string{"--no-pager"}
	for _, setting := range []string{"core.fsmonitor=false", "core.hooksPath=" + os.DevNull, "core.attributesFile=" + os.DevNull, "core.quotePath=true", "diff.external=", "diff.submodule=short", "log.showSignature=false", "credential.helper=", "protocol.allow=never", "gc.auto=0", "maintenance.auto=false"} {
		fixed = append(fixed, "-c", setting)
	}
	cmd := exec.CommandContext(ctx, "git", append(fixed, args...)...)
	cmd.Dir = root
	// Inherit neither Git overrides nor loader variables, credentials or user config.
	cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "LANG=C", "LC_ALL=C", "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_SYSTEM=" + os.DevNull, "GIT_CONFIG_GLOBAL=" + os.DevNull, "GIT_ATTR_NOSYSTEM=1", "GIT_OPTIONAL_LOCKS=0", "GIT_NO_REPLACE_OBJECTS=1", "GIT_NO_LAZY_FETCH=1", "GIT_TERMINAL_PROMPT=0"}
	cmd.WaitDelay = time.Second
	output := &gitInspectOutput{}
	cmd.Stdout, cmd.Stderr = output, io.Discard
	err := cmd.Run()
	return output.prefix, output.seen, err
}

func gitInspectFailure(ctx context.Context, err error, code, message string) ToolOutcome {
	if errors.Is(ctx.Err(), context.DeadlineExceeded) {
		return failOutcome("timeout", "Git inspection exceeded its deadline")
	}
	if ctx.Err() != nil {
		return failOutcome("cancelled", "Git inspection was cancelled")
	}
	if errors.Is(err, exec.ErrNotFound) {
		return failOutcome("git_unavailable", "Git is not installed or available on PATH")
	}
	// stderr can contain withheld paths or configuration values.
	return failOutcome(code, message)
}
