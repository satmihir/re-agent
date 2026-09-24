package reagent

import (
	"bytes"
	"os"
	"strings"
	"testing"
)

func TestDisplayWidth(t *testing.T) {
	for _, test := range []struct {
		text string
		want int
	}{{"abc", 3}, {"界", 2}, {"🙂", 2}, {"e\u0301", 1}, {ansiBold + "ab" + ansiReset, 2}} {
		if got := displayWidth(test.text); got != test.want {
			t.Fatalf("displayWidth(%q) = %d, want %d", test.text, got, test.want)
		}
	}
}

func TestTruncateWidth(t *testing.T) {
	if got := truncateWidth("界abc", 4); got != "界a…" {
		t.Fatalf("got %q", got)
	}
	if got := truncateWidth("e\u0301abc", 3); got != "e\u0301a…" {
		t.Fatalf("got %q", got)
	}
}

func TestSanitize_KeepsTextAndEscapesControls(t *testing.T) {
	cases := map[string]struct{ in, want string }{
		"plain":      {"hello world", "hello world"},
		"newline":    {"a\nb", "a\nb"},
		"tab":        {"a\tb", "a\tb"},
		"unicode":    {"héllo 日本語 🙂", "héllo 日本語 🙂"},
		"escape":     {"\x1b[31mred", `\x1b[31mred`},
		"carriage":   {"visible\rhidden", `visible\x0dhidden`},
		"bell":       {"ping\x07", `ping\x07`},
		"delete":     {"a\x7fb", `a\x7fb`},
		"c1 control": {"a\u0085b", `a\x85b`},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			if got := sanitize(c.in); got != c.want {
				t.Fatalf("got %q, want %q", got, c.want)
			}
		})
	}
}

// The reason sanitizing runs before styling: a model quoting a file full of
// escape sequences must not be able to drive the terminal (v1 §18.3).
func TestDisplay_ModelEscapeSequencesNeverReachTheTerminal(t *testing.T) {
	// A reply that read a file containing a colour code and a screen clear.
	reply := "The file says \x1b[31mDANGER\x1b[0m and \x1b[2J."

	unstyled := display(reply, false, 0)
	if strings.ContainsRune(unstyled, 0x1b) {
		t.Fatalf("an escape survived into unstyled output: %q", unstyled)
	}

	styled := display(reply, true, 0)
	for _, forbidden := range []string{"\x1b[31m", "\x1b[2J"} {
		if strings.Contains(styled, forbidden) {
			t.Fatalf("the model's own escape reached the terminal: %q", styled)
		}
	}
	if !strings.Contains(styled, `\x1b[31m`) {
		t.Fatalf("the escape was dropped rather than shown: %q", styled)
	}
}

// Only a terminal gets styling, so a pipe still carries the exact Markdown.
func TestDisplay_UnstyledOutputIsTheModelsOwnText(t *testing.T) {
	reply := "## Heading\n\nUse **bold** and `code` and [a](b)."
	if got := display(reply, false, 0); got != reply {
		t.Fatalf("piped output was altered:\ngot  %q\nwant %q", got, reply)
	}
}

func TestRenderMarkdown_Constructs(t *testing.T) {
	cases := map[string]struct{ in, contains string }{
		"heading":     {"## Title", ansiBold + "Title" + ansiReset},
		"bold":        {"a **b** c", ansiBold + "b" + ansiReset},
		"italic":      {"a *b* c", ansiItalic + "b" + ansiReset},
		"inline code": {"run `go test` now", ansiCode + "go test" + ansiReset},
		"bullet":      {"- an item", ansiDim + "•" + ansiReset + " an item"},
		"star bullet": {"* an item", ansiDim + "•" + ansiReset + " an item"},
		"numbered":    {"1. first", ansiDim + "1." + ansiReset + " first"},
		"quote":       {"> a remark", "│ " + ansiReset + "a remark"},
		"link":        {"see [docs](http://x)", "docs" + ansiDim + " (http://x)" + ansiReset},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			if got := renderMarkdown(c.in, 0); !strings.Contains(got, c.contains) {
				t.Fatalf("got %q, want it to contain %q", got, c.contains)
			}
		})
	}
}

// The form a model reaches for constantly. Splitting code spans out first used
// to tear this one in half.
func TestRenderMarkdown_LinkWithCodeText(t *testing.T) {
	got := renderMarkdown("See [`LICENSE`](LICENSE) for terms.", 0)
	if !strings.Contains(got, ansiCode+"LICENSE"+ansiReset) {
		t.Fatalf("the link text lost its code styling: %q", got)
	}
	if !strings.Contains(got, ansiDim+" (LICENSE)"+ansiReset) {
		t.Fatalf("the link target was not rendered: %q", got)
	}
	if strings.Contains(got, "](") {
		t.Fatalf("raw link markup survived: %q", got)
	}
}

// snake_case and dunders are ordinary text here, which is why underscore
// emphasis is not supported.
func TestRenderMarkdown_UnderscoresStayLiteral(t *testing.T) {
	line := "call some_var_name and __init__ directly"
	if got := renderMarkdown(line, 0); got != line {
		t.Fatalf("underscores were styled: %q", got)
	}
}

func TestRenderMarkdown_FenceContentIsNotMarkup(t *testing.T) {
	got := renderMarkdown("text\n```go\nx := **not bold**\n```\nafter", 0)
	if strings.Contains(got, ansiBold) {
		t.Fatalf("markup inside a fence was interpreted: %q", got)
	}
	if !strings.Contains(got, ansiCode+"x := **not bold**"+ansiReset) {
		t.Fatalf("fence content was not styled as code: %q", got)
	}
}

func TestRenderMarkdown_UnterminatedCodeSpan(t *testing.T) {
	got := renderMarkdown("an `unclosed span", 0)
	if strings.Contains(got, ansiCode) {
		t.Fatalf("an unpaired backtick opened a code span: %q", got)
	}
}

func TestRenderMarkdown_HashWithoutSpaceIsNotAHeading(t *testing.T) {
	line := "#hashtag stays text"
	if got := renderMarkdown(line, 0); got != line {
		t.Fatalf("got %q", got)
	}
}

func TestRenderMarkdown_PreservesIndentation(t *testing.T) {
	got := renderMarkdown("    - nested item", 0)
	if !strings.HasPrefix(got, "    ") {
		t.Fatalf("indentation was lost: %q", got)
	}
}

// stripStyles removes the harness's style sequences, leaving what a reader sees.
func stripStyles(text string) string {
	return styleSequencePattern.ReplaceAllString(text, "")
}

func TestWrapStyled_BreaksAtSpacesWithHangingIndent(t *testing.T) {
	reply := "- one two three four five six\n12. alpha beta gamma delta\n> quoted words wrap under the bar"
	got := renderMarkdown(reply, 20)
	want := "• one two three four\n  five six\n" +
		"12. alpha beta gamma\n    delta\n" +
		"│ quoted words wrap\n│ under the bar"
	if stripStyles(got) != want {
		t.Fatalf("got\n%s\nwant\n%s", stripStyles(got), want)
	}
	for _, line := range strings.Split(got, "\n") {
		if displayWidth(line) > 20 {
			t.Fatalf("%q is %d columns wide", line, displayWidth(line))
		}
	}
}

func TestWrapStyled_LongWordOverflows(t *testing.T) {
	got := wrapStyled("", "", "tiny supercalifragilistic end", 10)
	if strings.Join(got, "|") != "tiny|supercalifragilistic|end" {
		t.Fatalf("got %q", got)
	}
}

// A span broken across rows is closed at the end of one and reopened at the
// start of the next, so no row depends on the one before it.
func TestWrapStyled_StylingSurvivesTheBreak(t *testing.T) {
	got := wrapStyled("", "", "aa "+ansiBold+"bb cc"+ansiReset+" dd", 5)
	want := []string{"aa " + ansiBold + "bb" + ansiReset, ansiBold + "cc" + ansiReset + " dd"}
	if strings.Join(got, "\n") != strings.Join(want, "\n") {
		t.Fatalf("got %q, want %q", got, want)
	}

	// A quote's bar ends in a reset, which would otherwise cancel the bold.
	quoted := strings.Split(renderMarkdown("> aa **bb cc** dd", 7), "\n")
	if len(quoted) != 2 || !strings.HasPrefix(quoted[1], ansiDim+"│ "+ansiReset+ansiBold+"cc") {
		t.Fatalf("the bold did not resume after the bar: %q", quoted)
	}
}

func TestRenderMarkdown_FenceIsNeverWrapped(t *testing.T) {
	code := "x := someFunction(argumentOne, argumentTwo, argumentThree)"
	got := renderMarkdown("```go\n"+code+"\n```", 20)
	if !strings.Contains(got, ansiCode+code+ansiReset) {
		t.Fatalf("code inside a fence was rewrapped: %q", got)
	}
}

func TestRenderMarkdown_Table(t *testing.T) {
	table := "| Name | Size | Kind |\n" +
		"|:-----|-----:|:----:|\n" +
		"| a\\|b | 5 | 界 |\n" +
		"| longer | 12345 | x |"
	rule := strings.Repeat("─", 6) + "─┼─" + strings.Repeat("─", 5) + "─┼─" + strings.Repeat("─", 4)
	// Left, right, and centered columns; \| is a pipe inside a cell, and 界
	// counts as two columns.
	want := "Name   │  Size │ Kind\n" + rule + "\n" +
		"a|b    │     5 │  界\n" +
		"longer │ 12345 │  x\n" +
		"after"

	got := renderMarkdown(table+"\nafter", 0)
	if stripStyles(got) != want {
		t.Fatalf("got\n%s\nwant\n%s", stripStyles(got), want)
	}
	if !strings.Contains(got, ansiBold+"Name") {
		t.Fatalf("the header is not bold: %q", got)
	}

	// Aligned, the table is 21 columns wide. At 20 it is left as written.
	if got := stripStyles(renderMarkdown(table, 20)); got != table {
		t.Fatalf("a table too wide to align was altered:\n%s", got)
	}

	// A separator with a different number of cells does not make a table.
	notTable := "| a | b |\n|---|"
	if got := stripStyles(renderMarkdown(notTable, 0)); got != notTable {
		t.Fatalf("got %q", got)
	}
}

// Wrapping and tables are for a terminal. A pipe gets the reply as written.
func TestDisplay_PipedReplyIsUnchanged(t *testing.T) {
	reply := "| a | b |\n|---|---|\n| 1 | 2 |\n\n" + strings.Repeat("a long line of prose ", 10)
	if got := display(reply, false, 20); got != reply {
		t.Fatalf("piped output was altered:\n%s", got)
	}
}

func TestStyledOutput_OnlyForATerminal(t *testing.T) {
	if styledOutput(&bytes.Buffer{}) {
		t.Fatal("a buffer was treated as a terminal")
	}
	file, err := os.CreateTemp(t.TempDir(), "out")
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	if styledOutput(file) {
		t.Fatal("a redirect to a file was treated as a terminal")
	}
}

func TestStyledOutput_HonorsNoColor(t *testing.T) {
	t.Setenv("NO_COLOR", "1")
	if styledOutput(os.Stdout) {
		t.Fatal("NO_COLOR was ignored")
	}
}
