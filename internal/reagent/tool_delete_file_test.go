package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestDeleteFile_LargeBinaryDigest(t *testing.T) {
	content := bytes.Repeat([]byte{0, 0xff, 0x80, '\n'}, MaxFileBytes/4+1)
	ws := testWorkspace(t, map[string]string{"artifact.bin": string(content)})
	digest := digestOfFile(t, ws, "artifact.bin")
	for _, expected := range []string{strings.Repeat("0", 64), digest} {
		out := runTool(t, NewDeleteFileTool(ws), `{"path":"artifact.bin","expected_sha256":"`+expected+`"}`)
		if expected != digest {
			if out.Code != "unknown_digest" || out.Effect != EffectNone {
				t.Fatalf("wrong digest %+v", out)
			}
			got, err := os.ReadFile(filepath.Join(ws.Root(), "artifact.bin"))
			if err != nil || !bytes.Equal(got, content) {
				t.Fatal("wrong digest changed binary")
			}
		} else if !out.OK || out.Effect != EffectApplied {
			t.Fatalf("matching digest %+v", out)
		}
	}
	if _, err := os.Lstat(filepath.Join(ws.Root(), "artifact.bin")); !os.IsNotExist(err) {
		t.Fatal("binary remains")
	}
}

func TestDeleteFile_RecursiveNestedAndOutsideLinks(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"scratch/sub/a": "a", "scratch/b": "b", "keep": "keep"})
	outside := filepath.Join(t.TempDir(), "target")
	if err := os.WriteFile(outside, []byte("outside"), 0600); err != nil {
		t.Fatal(err)
	}
	for _, target := range []string{outside, filepath.Join(ws.Root(), "keep"), filepath.Join(ws.Root(), "missing")} {
		link := filepath.Join(ws.Root(), "scratch", fmt.Sprintf("link%d", len(target)))
		if err := os.Symlink(target, link); err != nil {
			t.Fatal(err)
		}
	}
	readDigest(t, ws, "scratch/sub/a")
	out := runTool(t, NewDeleteFileTool(ws), `{"path":"scratch","recursive":true}`)
	var result deleteTreeResult
	data(t, out, &result)
	if result.RemovedCount != 7 || result.Omitted != 0 || len(result.RemovedPaths) != 7 || out.Effect != EffectApplied {
		t.Fatalf("result %+v %+v", out, result)
	}
	if _, err := os.Lstat(filepath.Join(ws.Root(), "scratch")); !os.IsNotExist(err) {
		t.Fatal("scratch remains")
	}
	if got, err := os.ReadFile(outside); err != nil || string(got) != "outside" {
		t.Fatal("outside target changed")
	}
	if fileContent(t, ws, "keep") != "keep" {
		t.Fatal("inside target changed")
	}
	if len(ws.seen) != 0 {
		t.Fatal("removed observations retained")
	}
}

func TestDeleteFile_RecursiveRefusesUnsafeTreeBeforeEffects(t *testing.T) {
	for _, path := range []string{".", "", ".git", ".env", "scratch/../keep", "../outside", "scratch", "alias/sub"} {
		t.Run(path, func(t *testing.T) {
			ws := testWorkspace(t, map[string]string{"scratch/a": "a", "scratch/sub/.EnV.secret": "secret", "keep": "keep", ".git/config": "git", ".env": "secret"})
			if err := os.Symlink(filepath.Join(ws.Root(), "scratch"), filepath.Join(ws.Root(), "alias")); err != nil {
				t.Fatal(err)
			}
			raw, _ := json.Marshal(map[string]any{"path": path, "recursive": true})
			out, err := NewDeleteFileTool(ws).Execute(context.Background(), raw)
			if err != nil || out.OK || out.Effect != EffectNone {
				t.Fatalf("unsafe %+v %v", out, err)
			}
			if fileContent(t, ws, "scratch/a") != "a" || fileContent(t, ws, "keep") != "keep" {
				t.Fatal("preflight removed files")
			}
		})
	}
}

func TestDeleteFile_RecursivePartialFailureAndLedger(t *testing.T) {
	s := agentFixture(t)
	ws := s.workspace.active
	for _, name := range []string{"scratch/a", "scratch/b"} {
		if err := os.MkdirAll(filepath.Dir(filepath.Join(ws.Root(), name)), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(ws.Root(), name), []byte(name), 0600); err != nil {
			t.Fatal(err)
		}
		readDigest(t, ws, name)
	}
	tool := deleteFileTool{ws: ws, remove: func(path string) error {
		if strings.HasSuffix(path, "/a") {
			return os.ErrPermission
		}
		return os.Remove(path)
	}}
	s.cfg.Registry.byName["delete_file"] = tool
	s.model = NewScriptedModel(turn(callBlock("delete", "delete_file", `{"path":"scratch","recursive":true}`)), turn(textBlock("inspect partial failure")))
	run := agentTurn(t, s, "clean")
	out := results(s)[0].Outcome
	var result deleteTreeResult
	if err := json.Unmarshal(out.Data, &result); err != nil {
		t.Fatal(err)
	}
	if out.OK || out.Effect != EffectApplied || result.RemovedCount != 1 || result.RemovedPaths[0] != "scratch/b" || result.FailedPath != "scratch/a" {
		t.Fatalf("partial %+v %+v", out, result)
	}
	if len(run.Effects) != 1 || run.Effects[0].Path != "scratch/b" {
		t.Fatalf("ledger claims unremoved paths %+v", run.Effects)
	}
	if _, seen := ws.seen[filepath.Join(ws.Root(), "scratch/b")]; seen {
		t.Fatal("removed file digest retained")
	}
	if _, seen := ws.seen[filepath.Join(ws.Root(), "scratch/a")]; !seen {
		t.Fatal("failed file digest forgotten")
	}
	recap := recapLine(ToolCall{Name: "delete_file", Arguments: `{"path":"scratch"}`}, out)
	if strings.Contains(recap, "deleted scratch") || !strings.Contains(recap, "partial") {
		t.Fatalf("misleading recap %q", recap)
	}
}

func TestDeleteFile_RecursiveBoundedRemovedPaths(t *testing.T) {
	ws := testWorkspace(t, map[string]string{})
	if err := os.Mkdir(filepath.Join(ws.Root(), "scratch"), 0755); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 1200; i++ {
		path := filepath.Join(ws.Root(), "scratch", fmt.Sprintf("file%04d-%s", i, strings.Repeat("x", 20)))
		if err := os.WriteFile(path, nil, 0600); err != nil {
			t.Fatal(err)
		}
	}
	out := runTool(t, NewDeleteFileTool(ws), `{"path":"scratch","recursive":true}`)
	var result deleteTreeResult
	data(t, out, &result)
	if result.RemovedCount != 1201 || result.Omitted != 1201-len(result.RemovedPaths) || result.Omitted == 0 || !out.Truncated || encodedSize(out) > MaxResultBytes {
		t.Fatalf("bounded %+v %+v", out, result)
	}
	summary := summarizeAgentEffects([]EffectRecord{{Tool: "delete_file", Effect: EffectApplied, OmittedPaths: result.Omitted}})
	if summary.Omitted != result.Omitted {
		t.Fatal("omitted effects lost")
	}
}

func TestDeleteFile_RecursiveRegularFileKeepsDigestRules(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a": "before"})
	args := `{"path":"a","recursive":true}`
	if out := runTool(t, NewDeleteFileTool(ws), args); out.Code != "invalid_arguments" {
		t.Fatalf("unread %+v", out)
	}
	readDigest(t, ws, "a")
	if err := os.WriteFile(filepath.Join(ws.Root(), "a"), []byte("after"), 0600); err != nil {
		t.Fatal(err)
	}
	if out := runTool(t, NewDeleteFileTool(ws), args); out.Code != "stale_file" {
		t.Fatalf("stale %+v", out)
	}
	readDigest(t, ws, "a")
	if out := runTool(t, NewDeleteFileTool(ws), args); !out.OK {
		t.Fatalf("observed %+v", out)
	}
	if err := os.WriteFile(filepath.Join(ws.Root(), "a"), []byte("after"), 0600); err != nil {
		t.Fatal(err)
	}
	if out := runTool(t, NewDeleteFileTool(ws), args); out.Code != "invalid_arguments" {
		t.Fatalf("recreated retains permission %+v", out)
	}
}

func TestAgents_WriterRecursivePartialEffectsReachRoot(t *testing.T) {
	s := agentFixture(t)
	root := s.workspace.active.Root()
	if err := os.Mkdir(filepath.Join(root, "scratch"), 0755); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"a", "b"} {
		if err := os.WriteFile(filepath.Join(root, "scratch", name), []byte(name), 0600); err != nil {
			t.Fatal(err)
		}
	}
	s.cfg.Registry.byName["delete_file"] = deleteFileTool{ws: s.workspace.active, remove: func(path string) error {
		if filepath.Base(path) == "a" {
			return os.ErrPermission
		}
		return os.Remove(path)
	}}
	s.agentModel = func(Config, *Trace) (Model, error) {
		return NewScriptedModel(turn(callBlock("d", "delete_file", `{"path":"scratch","recursive":true}`)), turn(textBlock(strings.Repeat("reply", 10000)))), nil
	}
	s.model = NewScriptedModel(turn(writeSpawnBlock("spawn", "clean scratch", "writer"), callBlock("w", "wait", `{"ids":["writer"]}`)), turn(textBlock("inspected")))
	run := agentTurn(t, s, "clean")
	deliveries := agentResults(s.history)
	if len(deliveries) != 1 || !deliveries[0].Truncated || len(deliveries[0].Effects.Files) != 1 || deliveries[0].Effects.Files[0].Path != "scratch/b" {
		t.Fatalf("delivery %+v", deliveries)
	}
	if len(run.Effects) != 1 || run.Effects[0].Path != "scratch/b" || run.Effects[0].AgentID != "t1" {
		t.Fatalf("root effects %+v", run.Effects)
	}
	if got, err := os.ReadFile(filepath.Join(root, "scratch", "a")); err != nil || string(got) != "a" {
		t.Fatal("failed deletion target changed")
	}
}
