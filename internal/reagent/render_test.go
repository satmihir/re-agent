package reagent

import (
	"bytes"
	"os"
	"strings"
	"testing"
)

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

	unstyled := display(reply, false)
	if strings.ContainsRune(unstyled, 0x1b) {
		t.Fatalf("an escape survived into unstyled output: %q", unstyled)
	}

	styled := display(reply, true)
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
	if got := display(reply, false); got != reply {
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
			if got := renderMarkdown(c.in); !strings.Contains(got, c.contains) {
				t.Fatalf("got %q, want it to contain %q", got, c.contains)
			}
		})
	}
}

// The form a model reaches for constantly. Splitting code spans out first used
// to tear this one in half.
func TestRenderMarkdown_LinkWithCodeText(t *testing.T) {
	got := renderMarkdown("See [`LICENSE`](LICENSE) for terms.")
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
	if got := renderMarkdown(line); got != line {
		t.Fatalf("underscores were styled: %q", got)
	}
}

func TestRenderMarkdown_FenceContentIsNotMarkup(t *testing.T) {
	got := renderMarkdown("text\n```go\nx := **not bold**\n```\nafter")
	if strings.Contains(got, ansiBold) {
		t.Fatalf("markup inside a fence was interpreted: %q", got)
	}
	if !strings.Contains(got, ansiCode+"x := **not bold**"+ansiReset) {
		t.Fatalf("fence content was not styled as code: %q", got)
	}
}

func TestRenderMarkdown_UnterminatedCodeSpan(t *testing.T) {
	got := renderMarkdown("an `unclosed span")
	if strings.Contains(got, ansiCode) {
		t.Fatalf("an unpaired backtick opened a code span: %q", got)
	}
}

func TestRenderMarkdown_HashWithoutSpaceIsNotAHeading(t *testing.T) {
	line := "#hashtag stays text"
	if got := renderMarkdown(line); got != line {
		t.Fatalf("got %q", got)
	}
}

func TestRenderMarkdown_PreservesIndentation(t *testing.T) {
	got := renderMarkdown("    - nested item")
	if !strings.HasPrefix(got, "    ") {
		t.Fatalf("indentation was lost: %q", got)
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
