package reagent

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"testing"
)

var originalTestCache, originalTestHome string

// TestMain isolates default traces and checkpoints, including direct calls to Main.
func TestMain(m *testing.M) {
	cache, err := os.UserCacheDir()
	if err != nil {
		panic(err)
	}
	originalTestCache = resolvedTestPath(cache)
	originalTestHome = os.Getenv("HOME")
	testCachePath = guardTestCachePath
	home, err := os.MkdirTemp("", "reagent-test-home-")
	if err != nil {
		panic(err)
	}
	for key, value := range map[string]string{"HOME": home, "XDG_CACHE_HOME": filepath.Join(home, ".cache")} {
		if err := os.Setenv(key, value); err != nil {
			os.RemoveAll(home)
			panic(err)
		}
	}
	code := m.Run()
	if err := os.RemoveAll(home); err != nil {
		fmt.Fprintln(os.Stderr, "remove test HOME:", err)
		code = 1
	}
	os.Exit(code)
}

func setTestHome(t *testing.T, home string) {
	t.Helper()
	t.Setenv("HOME", home)
	t.Setenv("XDG_CACHE_HOME", filepath.Join(home, ".cache"))
}

// Resolve components before cleaning: link/.. follows the link, then its parent.
func resolvedTestPath(path string) string {
	if !filepath.IsAbs(path) {
		cwd, err := os.Getwd()
		if err != nil {
			panic(err)
		}
		path = cwd + string(filepath.Separator) + path
	}
	volume := filepath.VolumeName(path)
	resolved := volume + string(filepath.Separator)
	for _, part := range strings.Split(strings.TrimPrefix(path, volume), string(filepath.Separator)) {
		if part == "" || part == "." {
			continue
		}
		next := resolved + string(filepath.Separator) + part
		target, err := filepath.EvalSymlinks(next)
		if err == nil {
			resolved = target
			continue
		}
		if !os.IsNotExist(err) && !errors.Is(err, syscall.ENOTDIR) {
			panic(err)
		}
		// Missing suffixes and intentionally invalid trace destinations are still checked.
		resolved = filepath.Join(resolved, part)
	}
	return resolved
}

func guardTestCachePath(path string) {
	resolved := resolvedTestPath(path)
	rel, err := filepath.Rel(originalTestCache, resolved)
	if err != nil {
		panic(err)
	}
	inside := rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
	// SameFile catches case aliases without assuming the volume is case-insensitive.
	cacheInfo, cacheErr := os.Stat(originalTestCache)
	if os.IsNotExist(cacheErr) {
		// Until it exists, conservatively forbid case aliases on any volume.
		inside = inside || strings.HasPrefix(strings.ToLower(resolved)+string(filepath.Separator), strings.ToLower(originalTestCache)+string(filepath.Separator))
	}
	for probe := resolved; !inside && cacheErr == nil; probe = filepath.Dir(probe) {
		info, err := os.Stat(probe)
		inside = err == nil && os.SameFile(cacheInfo, info)
		if filepath.Dir(probe) == probe {
			break
		}
	}
	if !inside {
		return
	}
	// Include all stacks so an agent goroutine's failure names its owning test.
	stack := make([]byte, 1<<20)
	n := runtime.Stack(stack, true)
	panic(fmt.Sprintf("test cache guard: %s resolves under real user cache %s\n%s", path, originalTestCache, stack[:n]))
}

func TestCacheGuard_DefaultPathsAreIsolated(t *testing.T) {
	path, err := DefaultTracePath(NewID())
	if err != nil {
		t.Fatal(err)
	}
	trace := OpenTrace(path, "session", "run", io.Discard)
	trace.Close()
	if _, err := os.Stat(path); err != nil {
		t.Fatal(err)
	}
	store, err := openSessionStore(NewID(), true)
	if err != nil {
		t.Fatal(err)
	}
	defer store.close()
	if err := store.save(chatCheckpoint{ID: filepath.Base(store.dir)}); err != nil {
		t.Fatal(err)
	}
}

func TestCacheGuard_RejectsRealCache(t *testing.T) {
	alias := filepath.Join(t.TempDir(), "cache-link")
	// Link to the nearest existing ancestor without creating anything in the cache.
	ancestor := originalTestCache
	for {
		if _, err := os.Stat(ancestor); err == nil {
			break
		}
		ancestor = filepath.Dir(ancestor)
	}
	if err := os.Symlink(ancestor, alias); err != nil {
		t.Fatal(err)
	}
	rel, err := filepath.Rel(ancestor, originalTestCache)
	if err != nil {
		t.Fatal(err)
	}
	for _, root := range []string{originalTestCache, filepath.Join(alias, rel)} {
		for _, kind := range []string{"trace", "session", "save"} {
			t.Run(kind, func(t *testing.T) {
				defer func() {
					failure := fmt.Sprint(recover())
					if !strings.Contains(failure, "test cache guard:") || !strings.Contains(failure, "TestCacheGuard_RejectsRealCache") {
						t.Fatalf("expected guard panic naming test, got %s", failure)
					}
				}()
				if kind == "trace" {
					OpenTrace(filepath.Join(root, "reagent", "runs", NewID(), "events.jsonl"), "s", "r", io.Discard)
					return
				}
				if kind == "save" {
					store := &sessionStore{dir: filepath.Join(root, "reagent", "sessions", NewID())}
					store.save(chatCheckpoint{})
					return
				}
				// This also catches Linux's XDG_CACHE_HOME overriding an isolated HOME.
				t.Setenv("XDG_CACHE_HOME", root)
				if runtime.GOOS == "darwin" {
					t.Setenv("HOME", originalTestHome)
				}
				openSessionStore(NewID(), true)
			})
		}
	}
}

func TestCacheGuard_RejectsFilesystemAliases(t *testing.T) {
	cache := filepath.Join(t.TempDir(), "ForbiddenCache")
	child := filepath.Join(cache, "child")
	if err := os.MkdirAll(child, 0700); err != nil {
		t.Fatal(err)
	}
	original := originalTestCache
	originalTestCache = resolvedTestPath(cache)
	t.Cleanup(func() { originalTestCache = original })
	link := filepath.Join(t.TempDir(), "link")
	if err := os.Symlink(child, link); err != nil {
		t.Fatal(err)
	}
	out := filepath.Join(cache, "out")
	if err := os.Symlink(t.TempDir(), out); err != nil {
		t.Fatal(err)
	}
	paths := map[string]string{
		"cleaned_mkdir":  out + "/../new/events.jsonl",
		"symlink_parent": link + "/../events.jsonl",
		"missing_suffix": link + "/../missing/events.jsonl",
		"case_alias":     filepath.Join(filepath.Dir(cache), "forbiddencache", "events.jsonl"),
	}
	for name, path := range paths {
		t.Run(name, func(t *testing.T) {
			if name == "case_alias" {
				info, err := os.Stat(filepath.Dir(path))
				originalInfo, _ := os.Stat(cache)
				if err != nil || !os.SameFile(info, originalInfo) {
					t.Skip("case-sensitive volume")
				}
			}
			defer func() {
				failure := fmt.Sprint(recover())
				if !strings.Contains(failure, "test cache guard:") || !strings.Contains(failure, "TestCacheGuard_RejectsFilesystemAliases") {
					t.Fatalf("expected guard panic, got %s", failure)
				}
				for _, rel := range []string{"events.jsonl", "new", "missing"} {
					if _, err := os.Stat(filepath.Join(cache, rel)); !os.IsNotExist(err) {
						t.Fatal("guard allowed a cache write")
					}
				}
			}()
			OpenTrace(path, "s", "r", io.Discard)
		})
	}
}

func TestCacheGuard_RejectsAbsentCacheCaseAlias(t *testing.T) {
	parent := t.TempDir()
	original := originalTestCache
	originalTestCache = resolvedTestPath(filepath.Join(parent, "ForbiddenCache"))
	t.Cleanup(func() { originalTestCache = original })
	defer func() {
		failure := fmt.Sprint(recover())
		if !strings.Contains(failure, "test cache guard:") {
			t.Fatalf("expected guard panic, got %s", failure)
		}
		entries, err := os.ReadDir(parent)
		if err != nil || len(entries) != 0 {
			t.Fatalf("guard created files in absent cache: %v, %v", entries, err)
		}
	}()
	OpenTrace(filepath.Join(parent, "forbiddencache", "events.jsonl"), "s", "r", io.Discard)
}
