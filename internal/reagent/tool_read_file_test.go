package reagent

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"strings"
	"testing"
)

const fiveLines = "one\ntwo\nthree\nfour\nfive\n"

func TestReadFile_RangeCarriesDigestAndContinuation(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": fiveLines})
	outcome := runTool(t, NewReadFileTool(ws), `{"path":"a.txt","start_line":2,"max_lines":2}`)

	var got readFileResult
	data(t, outcome, &got)

	sum := sha256.Sum256([]byte(fiveLines))
	if got.SHA256 != hex.EncodeToString(sum[:]) {
		t.Fatalf("digest %s does not describe the file's bytes", got.SHA256)
	}
	if got.TotalLines != 5 || got.SizeBytes != len(fiveLines) {
		t.Fatalf("got %+v", got)
	}
	if len(got.Lines) != 2 || got.Lines[0] != (readLine{2, "two"}) || got.Lines[1] != (readLine{3, "three"}) {
		t.Fatalf("got %+v", got.Lines)
	}
	if got.NextLine == nil || *got.NextLine != 4 || got.EOF || !outcome.Truncated {
		t.Fatalf("continuation is wrong: %+v truncated=%v", got, outcome.Truncated)
	}
}

func TestReadFile_WholeFileReachesEOF(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": fiveLines})
	outcome := runTool(t, NewReadFileTool(ws), `{"path":"a.txt"}`)

	var got readFileResult
	data(t, outcome, &got)
	if len(got.Lines) != 5 || !got.EOF || got.NextLine != nil || outcome.Truncated {
		t.Fatalf("got %+v truncated=%v", got, outcome.Truncated)
	}
}

// An empty file is the one range past the last line that is not an error.
func TestReadFile_EmptyFileIsNotOutOfRange(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"empty.txt": ""})
	var got readFileResult
	data(t, runTool(t, NewReadFileTool(ws), `{"path":"empty.txt"}`), &got)

	if got.TotalLines != 0 || len(got.Lines) != 0 || !got.EOF {
		t.Fatalf("got %+v", got)
	}
}

func TestReadFile_CRLFIsStrippedForDisplayOnly(t *testing.T) {
	content := "a\r\nb"
	ws := testWorkspace(t, map[string]string{"a.txt": content})
	var got readFileResult
	data(t, runTool(t, NewReadFileTool(ws), `{"path":"a.txt"}`), &got)

	if got.Lines[0].Text != "a" || got.Lines[1].Text != "b" {
		t.Fatalf("got %+v", got.Lines)
	}
	sum := sha256.Sum256([]byte(content))
	if got.SHA256 != hex.EncodeToString(sum[:]) {
		t.Fatal("digest was taken over normalized text rather than the original bytes")
	}
}

func TestReadFile_Errors(t *testing.T) {
	ws := testWorkspace(t, map[string]string{
		"a.txt":     fiveLines,
		"sub/b.txt": "x",
		"binary":    "before\x00after",
		"bad-utf8":  "\xff\xfe",
	})
	cases := map[string]struct{ args, code string }{
		"missing file":    {`{"path":"absent.txt"}`, "not_found"},
		"directory":       {`{"path":"sub"}`, "not_file"},
		"binary":          {`{"path":"binary"}`, "binary_file"},
		"invalid utf8":    {`{"path":"bad-utf8"}`, "invalid_utf8"},
		"past last line":  {`{"path":"a.txt","start_line":6}`, "line_out_of_range"},
		"zero start line": {`{"path":"a.txt","start_line":0}`, "invalid_arguments"},
		"null max lines":  {`{"path":"a.txt","max_lines":null}`, "invalid_arguments"},
		"escaping path":   {`{"path":"../outside"}`, "invalid_path"},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			if got := runTool(t, NewReadFileTool(ws), c.args); got.OK || got.Code != c.code {
				t.Fatalf("got %s (%s), want %s", got.Code, got.Message, c.code)
			}
		})
	}
}

// The result budget, not the requested range, is what finally bounds a read.
func TestReadFile_TrimsToTheResultBudget(t *testing.T) {
	line := strings.Repeat("x", 1000) + "\n"
	ws := testWorkspace(t, map[string]string{"big.txt": strings.Repeat(line, 100)})
	outcome := runTool(t, NewReadFileTool(ws), `{"path":"big.txt","max_lines":100}`)

	var got readFileResult
	data(t, outcome, &got)
	if len(got.Lines) == 0 || len(got.Lines) >= 100 {
		t.Fatalf("got %d lines, want a trimmed page", len(got.Lines))
	}
	if !outcome.Truncated || got.EOF || got.NextLine == nil {
		t.Fatalf("a trimmed page must say so: %+v truncated=%v", got, outcome.Truncated)
	}
	if *got.NextLine != len(got.Lines)+1 {
		t.Fatalf("next_line %d does not follow the %d returned lines", *got.NextLine, len(got.Lines))
	}
	if len(outcome.Data) > MaxResultBytes {
		t.Fatalf("result is %d bytes, over the budget", len(outcome.Data))
	}
}

// A line that cannot fit is an explicit error, never an empty page that cannot
// advance (v0 §4).
func TestReadFile_SingleLineTooLong(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"long.txt": strings.Repeat("x", MaxResultBytes+1)})
	if got := runTool(t, NewReadFileTool(ws), `{"path":"long.txt"}`); got.Code != "line_too_long" {
		t.Fatalf("got %s (%s)", got.Code, got.Message)
	}
}

func TestReadFile_FileTooLarge(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"huge.txt": strings.Repeat("a\n", MaxFileBytes)})
	got := runTool(t, NewReadFileTool(ws), `{"path":"huge.txt"}`)
	if got.Code != "file_too_large" || !strings.Contains(got.Message, fmt.Sprint(MaxFileBytes)) {
		t.Fatalf("got %s (%s)", got.Code, got.Message)
	}
}

func TestReadFile_MalformedArguments(t *testing.T) {
	ws := testWorkspace(t, map[string]string{"a.txt": fiveLines})
	for name, args := range map[string]string{
		"invalid json":  `{"path":`,
		"unknown field": `{"path":"a.txt","encoding":"utf8"}`,
		"wrong type":    `{"path":"a.txt","start_line":"2"}`,
	} {
		t.Run(name, func(t *testing.T) {
			if got := runTool(t, NewReadFileTool(ws), args); got.Code != "invalid_arguments" {
				t.Fatalf("got %s (%s)", got.Code, got.Message)
			}
		})
	}
}
