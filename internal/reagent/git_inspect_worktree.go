package reagent

import (
	"bytes"
	"context"
	"crypto/sha1"
	"crypto/sha256"
	"errors"
	"fmt"
	"hash"
	"io"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"unicode/utf8"
)

const (
	gitInspectContentLimit   = 1 << 20
	gitInspectInventoryLimit = 8 << 20
	gitInspectUntrackedLimit = 1000
)

type gitInspectFile struct {
	mode, oid string
	flags     string
	size      int64
	content   []byte
	note      string
}

// Inventory is bounded separately from the final result: a large index must not
// silently turn missing inventory entries into additions or deletions.
func gitInspectInventory(ctx context.Context, root string, args ...string) ([]byte, bool, error) {
	out := &gitInspectOutput{limit: gitInspectInventoryLimit}
	err := gitInspectRun(ctx, root, out, args...)
	return out.prefix, out.seen > len(out.prefix), err
}

func gitInspectWorktree(ctx context.Context, root string, a gitInspectArgs, rev string, stat bool) (*gitInspectOutput, error) {
	out := &gitInspectOutput{}
	format, _, err := gitInspectCommand(ctx, root, "rev-parse", "--show-object-format")
	if err != nil {
		return nil, err
	}
	algorithm := strings.TrimSpace(string(format))
	if algorithm != "sha1" && algorithm != "sha256" {
		return nil, fmt.Errorf("unsupported Git object format")
	}
	scope := gitInspectPaths(a.Paths)
	// ls-tree supports literal pathspecs but not exclusion/glob magic.
	treeScope := []string{"."}
	if len(a.Paths) > 0 {
		treeScope = nil
		for _, p := range a.Paths {
			treeScope = append(treeScope, ":(literal)"+path.Clean(p))
		}
	}
	baseRaw, truncated, err := gitInspectInventory(ctx, root, append([]string{"ls-tree", "-r", "-z", rev, "--"}, treeScope...)...)
	if err != nil {
		return nil, err
	}
	indexRaw, indexTruncated, err := gitInspectInventory(ctx, root, append([]string{"ls-files", "--stage", "--debug", "-z", "--"}, scope...)...)
	if err != nil {
		return nil, err
	}
	if truncated || indexTruncated {
		fmt.Fprintln(out, "[truncated: inventory exceeds byte limit; comparison unavailable]")
		out.incomplete = true
		return out, nil
	}
	base, err := gitInspectTree(baseRaw)
	if err != nil {
		return nil, err
	}
	index, err := gitInspectIndex(indexRaw)
	if err != nil {
		return nil, err
	}
	if err := gitInspectModuleAncestors(ctx, root, rev, a.Paths, base, index); err != nil {
		return nil, err
	}
	untracked, untrackedTruncated, err := gitInspectInventory(ctx, root, append([]string{"ls-files", "-z", "--others", "--exclude-standard", "--"}, scope...)...)
	if err != nil {
		return nil, err
	}
	paths := make(map[string]bool)
	modules := make(map[string]bool)
	for p, f := range base {
		paths[p] = true
		if f.mode == "160000" {
			modules[p] = true
		}
	}
	for p, f := range index {
		paths[p] = true
		if f.mode == "160000" {
			modules[p] = true
		}
	}
	selected := func(p string) bool {
		if !gitInspectSelected(p, a.Paths) {
			return false
		}
		for parent := path.Dir(p); parent != "."; parent = path.Dir(parent) {
			if modules[parent] {
				return false
			}
		}
		return true
	}
	untrackedPaths := make(map[string]bool)
	listed := 0
	if untrackedTruncated {
		// The captured prefix may end inside a filename.
		untracked = untracked[:bytes.LastIndexByte(untracked, 0)+1]
	}
	for _, raw := range bytes.Split(untracked, []byte{0}) {
		p := string(raw)
		if !selected(p) {
			continue
		}
		if listed >= gitInspectUntrackedLimit {
			untrackedTruncated = true
			break
		}
		listed++
		paths[p], untrackedPaths[p] = true, true
	}
	names := make([]string, 0, len(paths))
	for p := range paths {
		if selected(p) {
			names = append(names, p)
		}
	}
	sort.Strings(names)
	conversions, err := gitInspectConversions(ctx, root, names)
	if err != nil {
		return nil, err
	}
	for _, p := range names {
		if out.seen > MaxResultBytes {
			break
		}
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		b, i := base[p], index[p]
		if a.Operation == "status" && !strings.Contains(i.flags, "intent-to-add") && !strings.Contains(i.flags, "unmerged") {
			if gitInspectChange(b, i) != "" && b.mode != "160000" && i.mode != "160000" {
				if b.oid != "" {
					b, err = gitInspectReadBlob(ctx, root, b, algorithm)
					if err != nil {
						return nil, err
					}
				}
				if i.oid != "" {
					i, err = gitInspectReadBlob(ctx, root, i, algorithm)
					if err != nil {
						return nil, err
					}
				}
			}
			gitInspectStatus(out, "staged", p, b, i, "")
		}
		if i.flags != "" {
			fmt.Fprintf(out, "index %q: %s; comparison unknown\n", p, i.flags)
			continue
		}
		if b.mode == "160000" || i.mode == "160000" {
			fmt.Fprintf(out, "submodule %q: base=%s index=%s; working tree not inspected\n", p, b.oid, i.oid)
			continue
		}
		current := gitInspectReadWorking(ctx, root, p, algorithm)
		if current.note != "" {
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			fmt.Fprintf(out, "unknown %q: %s\n", p, current.note)
			continue
		}
		conversion := ""
		if conversions[p] && current.mode != "120000" && !gitInspectBinary(current.content) {
			conversion = "; comparison not filter-aware"
		}
		if a.Operation == "status" {
			if untrackedPaths[p] {
				fmt.Fprintf(out, "untracked %q%s\n", p, conversion)
				gitInspectSummary(out, gitInspectFile{}, current)
			} else {
				if i.oid != "" && gitInspectChange(i, current) != "" && (current.size > gitInspectContentLimit || gitInspectBinary(current.content) || current.mode == "") {
					i, err = gitInspectReadBlob(ctx, root, i, algorithm)
					if err != nil {
						return nil, err
					}
				}
				gitInspectStatus(out, "unstaged", p, i, current, conversion)
			}
			continue
		}
		if gitInspectChange(b, current) == "" {
			continue
		}
		fmt.Fprintf(out, "diff --git %s %s\n", gitInspectDiffName("a/"+p), gitInspectDiffName("b/"+p))
		fmt.Fprintf(out, "%s %q%s\n", gitInspectChange(b, current), p, conversion)
		if b.mode != current.mode {
			fmt.Fprintf(out, "mode %s -> %s\n", gitInspectMode(b), gitInspectMode(current))
		}
		if b.oid != "" {
			b, err = gitInspectReadBlob(ctx, root, b, algorithm)
			if err != nil {
				return nil, err
			}
		}
		if stat {
			gitInspectSummary(out, b, current)
			continue
		}
		if b.oid == current.oid {
			continue
		}
		if b.size > gitInspectContentLimit || current.size > gitInspectContentLimit || gitInspectBinary(b.content) || gitInspectBinary(current.content) {
			fmt.Fprintln(out, "changed (binary or oversized)")
			gitInspectSummary(out, b, current)
			continue
		}
		oldName, newName := "a/"+p, "b/"+p
		if b.mode == "" {
			oldName = "/dev/null"
		}
		if current.mode == "" {
			newName = "/dev/null"
		}
		patch, ok := gitInspectUnified(b.content, current.content, oldName, newName)
		if !ok {
			fmt.Fprintln(out, "changed (diff budget exceeded)")
			gitInspectSummary(out, b, current)
			continue
		}
		fmt.Fprint(out, patch)
	}
	if untrackedTruncated {
		fmt.Fprintln(out, "[truncated: untracked listing]")
		out.incomplete = true
	}
	return out, nil
}

// Scoped inventories omit gitlinks above the selected path. Inspect ancestors
// without descending into their trees, so narrowing cannot expose a submodule.
func gitInspectModuleAncestors(ctx context.Context, root, rev string, paths []string, base, index map[string]gitInspectFile) error {
	seen := make(map[string]bool)
	for _, p := range paths {
		for parent := path.Dir(path.Clean(p)); parent != "."; parent = path.Dir(parent) {
			if seen[parent] {
				continue
			}
			seen[parent] = true
			raw, truncated, err := gitInspectInventory(ctx, root, "ls-tree", "-z", rev, "--", ":(literal)"+parent)
			if err != nil {
				return fmt.Errorf("inspect gitlink ancestor: %w", err)
			}
			if truncated {
				return fmt.Errorf("gitlink ancestor inventory exceeds byte limit")
			}
			entries, err := gitInspectTree(raw)
			if err != nil {
				return err
			}
			if f := entries[parent]; f.mode == "160000" {
				base[parent] = f
			}
			// Exclude descendants, not the gitlink entry itself.
			glob := strings.NewReplacer(`\`, `\\`, `*`, `\*`, `?`, `\?`, `[`, `\[`, `]`, `\]`).Replace(parent)
			raw, truncated, err = gitInspectInventory(ctx, root, "ls-files", "--stage", "--debug", "-z", "--", ":(literal)"+parent, ":(exclude,glob)"+glob+"/**")
			if err != nil {
				return fmt.Errorf("inspect index gitlink ancestor: %w", err)
			}
			if truncated {
				return fmt.Errorf("index gitlink ancestor inventory exceeds byte limit")
			}
			entries, err = gitInspectIndex(raw)
			if err != nil {
				return err
			}
			if f := entries[parent]; f.mode == "160000" {
				index[parent] = f
			}
		}
	}
	return nil
}

func gitInspectSelected(p string, paths []string) bool {
	if !gitInspectPath(p) || p == "." {
		return false
	}
	if len(paths) == 0 {
		return true
	}
	for _, selected := range paths {
		selected = path.Clean(selected)
		if selected == "." || p == selected || strings.HasPrefix(p, selected+"/") {
			return true
		}
	}
	return false
}

func gitInspectTree(raw []byte) (map[string]gitInspectFile, error) {
	files := make(map[string]gitInspectFile)
	for _, record := range bytes.Split(raw, []byte{0}) {
		if len(record) == 0 {
			continue
		}
		header, p, ok := strings.Cut(string(record), "\t")
		fields := strings.Fields(header)
		if !ok || len(fields) != 3 || !gitInspectOID.MatchString(fields[2]) {
			return nil, fmt.Errorf("invalid tree inventory")
		}
		files[p] = gitInspectFile{mode: fields[0], oid: fields[2]}
	}
	return files, nil
}

func gitInspectIndex(raw []byte) (map[string]gitInspectFile, error) {
	files := make(map[string]gitInspectFile)
	for len(raw) > 0 {
		nul := bytes.IndexByte(raw, 0)
		if nul < 0 {
			return nil, fmt.Errorf("invalid index inventory")
		}
		header, p, ok := strings.Cut(string(raw[:nul]), "\t")
		fields := strings.Fields(header)
		raw = raw[nul+1:]
		flagsStart := bytes.Index(raw, []byte("flags: "))
		if !ok || len(fields) != 3 || !gitInspectOID.MatchString(fields[1]) || flagsStart < 0 {
			return nil, fmt.Errorf("invalid index inventory")
		}
		flagsEnd := bytes.IndexByte(raw[flagsStart:], '\n')
		if flagsEnd < 0 {
			return nil, fmt.Errorf("invalid index flags")
		}
		flags, err := strconv.ParseUint(strings.TrimSpace(string(raw[flagsStart+7:flagsStart+flagsEnd])), 16, 32)
		if err != nil {
			return nil, fmt.Errorf("invalid index flags: %w", err)
		}
		raw = raw[flagsStart+flagsEnd+1:]
		f := gitInspectFile{mode: fields[0], oid: fields[1]}
		var notes []string
		if flags&0x40000000 != 0 {
			notes = append(notes, "skip-worktree")
		}
		if flags&0x8000 != 0 {
			notes = append(notes, "assume-unchanged")
		}
		if flags&0x20000000 != 0 {
			notes = append(notes, "intent-to-add")
		}
		if fields[2] != "0" {
			notes = append(notes, "unmerged index")
		}
		f.flags = strings.Join(notes, "; ")
		files[p] = f
	}
	return files, nil
}

func gitInspectChange(old, current gitInspectFile) string {
	switch {
	case old.mode == "" && current.mode == "":
		return ""
	case old.mode == "":
		return "added"
	case current.mode == "":
		return "deleted"
	case old.mode != current.mode || old.oid != current.oid:
		return "modified"
	default:
		return ""
	}
}

func gitInspectMode(f gitInspectFile) string {
	if f.mode == "" {
		return "000000"
	}
	return f.mode
}

func gitInspectStatus(out io.Writer, section, p string, old, current gitInspectFile, note string) {
	change := gitInspectChange(old, current)
	if change == "" {
		return
	}
	fmt.Fprintf(out, "%s %s %q%s\n", section, change, p, note)
	if old.mode != current.mode {
		fmt.Fprintf(out, "mode %s -> %s\n", gitInspectMode(old), gitInspectMode(current))
	}
	fmt.Fprintf(out, "blob %s -> %s\n", old.oid, current.oid)
	if old.size > gitInspectContentLimit || current.size > gitInspectContentLimit || gitInspectBinary(old.content) || gitInspectBinary(current.content) {
		gitInspectSummary(out, old, current)
	}
}

func gitInspectSummary(out io.Writer, old, current gitInspectFile) {
	fmt.Fprintf(out, "changed: old size=%d blob=%s; new size=%d blob=%s\n", old.size, old.oid, current.size, current.oid)
}

func gitInspectBinary(content []byte) bool {
	return bytes.IndexByte(content, 0) >= 0 || !utf8.Valid(content)
}

// Keep no content at all above the cap, but hash the entire byte stream. Git's
// blob header includes its length, so a concurrent size change is unknown.
func gitInspectReadContent(ctx context.Context, reader io.Reader, size int64, algorithm string) (gitInspectFile, error) {
	var digest hash.Hash = sha1.New()
	if algorithm == "sha256" {
		digest = sha256.New()
	}
	fmt.Fprintf(digest, "blob %d\x00", size)
	f := gitInspectFile{size: size}
	if size <= gitInspectContentLimit {
		f.content = make([]byte, 0, int(size))
	}
	buffer := make([]byte, 32<<10)
	var seen int64
	for {
		if err := ctx.Err(); err != nil {
			return f, err
		}
		n, err := reader.Read(buffer)
		if n > 0 {
			seen += int64(n)
			digest.Write(buffer[:n])
			if seen <= gitInspectContentLimit && size <= gitInspectContentLimit {
				f.content = append(f.content, buffer[:n]...)
			} else {
				f.content = nil
			}
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return f, err
		}
	}
	if seen != size {
		return f, fmt.Errorf("file size changed during comparison")
	}
	f.oid = fmt.Sprintf("%x", digest.Sum(nil))
	return f, nil
}

func gitInspectReadWorking(ctx context.Context, root, p, algorithm string) gitInspectFile {
	// Match the workspace's non-sandbox path model, but never traverse a known
	// symlink ancestor, nor open a symlink leaf as ordinary file content.
	parts := strings.Split(p, "/")
	abs := root
	for _, component := range parts[:len(parts)-1] {
		abs = filepath.Join(abs, component)
		info, err := os.Lstat(abs)
		if os.IsNotExist(err) {
			return gitInspectFile{}
		}
		if err != nil || !info.IsDir() {
			return gitInspectFile{note: "non-directory or symlink ancestor; not inspected"}
		}
	}
	abs = filepath.Join(root, filepath.FromSlash(p))
	info, err := os.Lstat(abs)
	if os.IsNotExist(err) {
		return gitInspectFile{}
	}
	if err != nil {
		return gitInspectFile{note: "file unavailable"}
	}
	if info.Mode()&os.ModeSymlink != 0 {
		target, err := os.Readlink(abs)
		if err != nil {
			return gitInspectFile{note: "symlink unavailable"}
		}
		f, err := gitInspectReadContent(ctx, strings.NewReader(target), int64(len(target)), algorithm)
		if err != nil {
			return gitInspectFile{note: "symlink changed during comparison"}
		}
		f.mode = "120000"
		return f
	}
	if !info.Mode().IsRegular() {
		return gitInspectFile{note: "not a regular file or symlink; not inspected"}
	}
	file, err := os.OpenFile(abs, os.O_RDONLY|syscall.O_NOFOLLOW|syscall.O_NONBLOCK, 0)
	if err != nil {
		return gitInspectFile{note: "file unavailable"}
	}
	defer file.Close()
	opened, err := file.Stat()
	if err != nil || !opened.Mode().IsRegular() || !os.SameFile(info, opened) {
		return gitInspectFile{note: "file changed during comparison"}
	}
	f, err := gitInspectReadContent(ctx, file, opened.Size(), algorithm)
	if err != nil {
		return gitInspectFile{note: "file unreadable or changed during comparison"}
	}
	after, err := file.Stat()
	if err != nil || after.Size() != opened.Size() || after.ModTime() != opened.ModTime() {
		return gitInspectFile{note: "file changed during comparison"}
	}
	f.mode = "100644"
	if opened.Mode()&0111 != 0 {
		f.mode = "100755"
	}
	return f
}

func gitInspectReadBlob(ctx context.Context, root string, f gitInspectFile, algorithm string) (gitInspectFile, error) {
	raw, _, err := gitInspectCommand(ctx, root, "cat-file", "-s", f.oid)
	if err != nil {
		return f, err
	}
	size, err := strconv.ParseInt(strings.TrimSpace(string(raw)), 10, 64)
	if err != nil || size < 0 {
		return f, fmt.Errorf("invalid blob size")
	}
	f.size = size
	if size > gitInspectContentLimit {
		// The object ID already names the complete blob; no content is needed.
		return f, nil
	}
	out := &gitInspectOutput{limit: gitInspectContentLimit}
	if err := gitInspectRun(ctx, root, out, "cat-file", "blob", f.oid); err != nil {
		return f, err
	}
	content, err := gitInspectReadContent(ctx, bytes.NewReader(out.prefix), size, algorithm)
	if err != nil || content.oid != f.oid || int64(out.seen) != size {
		return f, fmt.Errorf("blob digest mismatch")
	}
	content.mode, content.flags = f.mode, f.flags
	return content, nil
}

func gitInspectConversions(ctx context.Context, root string, names []string) (map[string]bool, error) {
	result := make(map[string]bool)
	configured := false
	for _, key := range []string{"core.autocrlf", "core.eol"} {
		raw, _, err := gitInspectCommand(ctx, root, "config", "--get", key)
		if err != nil {
			var exit *exec.ExitError
			if !errors.As(err, &exit) || exit.ExitCode() != 1 {
				return nil, err
			}
		}
		value := strings.ToLower(strings.TrimSpace(string(raw)))
		if (key == "core.autocrlf" && value != "" && value != "false") || (key == "core.eol" && value != "" && value != "native") {
			configured = true
		}
	}
	// Batch paths to bound argv and attribute output independently of the index.
	for start := 0; start < len(names); start += 32 {
		batch := names[start:min(start+32, len(names))]
		args := []string{"check-attr", "-z", "filter", "text", "eol", "crlf", "ident", "working-tree-encoding", "--"}
		raw, seen, err := gitInspectCommand(ctx, root, append(args, batch...)...)
		if err != nil {
			return nil, err
		}
		for _, p := range batch {
			result[p] = configured || seen > len(raw)
		}
		fields := bytes.Split(raw, []byte{0})
		for i := 0; i+2 < len(fields); i += 3 {
			value := string(fields[i+2])
			if value != "unspecified" && value != "unset" {
				result[string(fields[i])] = true
			}
		}
	}
	return result, nil
}
