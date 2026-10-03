package reagent

import (
	"bytes"
	"strings"
	"testing"
)

func welcomeConfig(t *testing.T) Config {
	t.Helper()
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.ReasoningEffort = openaiName, "gpt-6-luna", "low"
	return cfg
}

// The art is a grid: every row is the same width, and liquid only sits in
// columns that have a shade.
func TestFlaskArt_IsAGrid(t *testing.T) {
	for i, row := range flaskArt {
		runes := []rune(row)
		if len(runes) != flaskWidth {
			t.Fatalf("row %d is %d runes wide, want %d", i, len(runes), flaskWidth)
		}
		for col, r := range runes {
			if (r == 'L' || r == '>') && liquidShades[col] == 0 {
				t.Fatalf("row %d column %d is liquid with no shade", i, col)
			}
		}
	}
}

func TestWelcome_DrawsTheFlaskBesideTheDetails(t *testing.T) {
	var out bytes.Buffer
	d := &Display{w: &out, styled: true}
	d.welcome(welcomeConfig(t), "/work/repo", "", 100)

	lines := strings.Split(strings.TrimSuffix(out.String(), "\n"), "\n")
	if len(lines) != len(flaskArt) {
		t.Fatalf("got %d lines, want %d:\n%s", len(lines), len(flaskArt), out.String())
	}
	plain := stripANSI(out.String())
	for _, want := range []string{"re:agent", "gpt-6-luna · openai · effort low", "/work/repo",
		"read, write, and execute", "/help commands", "❯", "●"} {
		if !strings.Contains(plain, want) {
			t.Fatalf("missing %q in:\n%s", want, plain)
		}
	}
	if strings.Contains(plain, "steps") || strings.Contains(plain, "tool calls") {
		t.Fatalf("welcome shows turn budgets:\n%s", plain)
	}
	// The colon carries the logo's purple, and the liquid its shades.
	if !strings.Contains(out.String(), ansiBubble+ansiBold+":") || !strings.Contains(out.String(), "\x1b[48;5;17m") {
		t.Fatalf("colours missing: %q", out.String())
	}
}

func TestHeader_ShowsLoadedProjectInstructionsOnlyInChat(t *testing.T) {
	cfg := welcomeConfig(t)
	project := strings.Repeat("x", 5036)
	cfg.ProjectInstructions = &project
	var plain, run, styled bytes.Buffer
	(&Display{w: &plain}).header(cfg, "/work/repo", "", true)
	(&Display{w: &run}).header(cfg, "/work/repo", "", false)
	(&Display{w: &styled, styled: true}).welcome(cfg, "/work/repo", "", welcomeColumns)
	label := "AGENTS.md loaded (5.0 KB)"
	if !strings.Contains(plain.String(), label) || !strings.Contains(stripANSI(styled.String()), label) || strings.Contains(run.String(), label) {
		t.Fatalf("plain %q, run %q, styled %q", plain.String(), run.String(), stripANSI(styled.String()))
	}
	for _, row := range strings.Split(strings.TrimSuffix(styled.String(), "\n"), "\n") {
		if displayWidth(row) >= welcomeColumns {
			t.Fatalf("welcome wrapped: %q", stripANSI(row))
		}
	}
}

func TestWelcome_FitsTheTerminal(t *testing.T) {
	var out bytes.Buffer
	d := &Display{w: &out, styled: true}
	long := "/work/" + strings.Repeat("very-long-directory-name/", 8) + "repo"
	d.welcome(welcomeConfig(t), long, "http://proxy.example:8080/v1/responses/"+strings.Repeat("x", 80), welcomeColumns)

	for _, line := range strings.Split(strings.TrimSuffix(out.String(), "\n"), "\n") {
		if width := displayWidth(line); width >= welcomeColumns {
			t.Fatalf("a %d-cell line on a %d-column terminal: %q", width, welcomeColumns, stripANSI(line))
		}
	}
	if !strings.Contains(stripANSI(out.String()), "requests go to http://proxy.example") {
		t.Fatalf("proxy line missing:\n%s", stripANSI(out.String()))
	}
}

func TestWelcome_PlanModeIsTealWithoutWideningRow(t *testing.T) {
	cfg := welcomeConfig(t)
	cfg.PlanMode = true
	var out bytes.Buffer
	(&Display{w: &out, styled: true}).welcome(cfg, "/work/repo", "", welcomeColumns)
	if !strings.Contains(out.String(), ansiDim+"read, write, and execute · "+ansiPromptTeal+"plan mode"+"\x1b[22;39m"+ansiDim) {
		t.Fatalf("plan mode is not teal within the dim details: %q", out.String())
	}
	for _, row := range strings.Split(strings.TrimSuffix(out.String(), "\n"), "\n") {
		if displayWidth(row) >= welcomeColumns {
			t.Fatalf("welcome wrapped: %q", row)
		}
	}
	var plain bytes.Buffer
	(&Display{w: &plain}).header(cfg, "/work/repo", "", true)
	if strings.Contains(plain.String(), "\x1b[") || !strings.Contains(plain.String(), "· plan mode") {
		t.Fatalf("plain header: %q", plain.String())
	}
}

func TestHeader_RunPlanModeReplacesExecNotice(t *testing.T) {
	cfg := welcomeConfig(t)
	cfg.PlanMode = true
	var out bytes.Buffer
	(&Display{w: &out}).header(cfg, "/work/repo", "", false)
	for _, want := range []string{
		"re:agent · gpt-6-luna (openai, effort low) · read, write, and execute · plan mode · /work/repo\n",
		"! plan mode: exec and file changes are refused while plan mode is on\n",
	} {
		if !strings.Contains(out.String(), want) {
			t.Fatalf("missing %q in %q", want, out.String())
		}
	}
	if strings.Contains(out.String(), "! exec mode:") {
		t.Fatalf("run in plan mode claims exec is available: %q", out.String())
	}
}

func TestHeader_PlainChatPlanModeReplacesExecNotice(t *testing.T) {
	cfg := welcomeConfig(t)
	cfg.PlanMode = true
	var out bytes.Buffer
	(&Display{w: &out}).header(cfg, "/work/repo", "", true)
	for _, want := range []string{
		"workspace /work/repo · read, write, and execute · plan mode · 20 steps, 40 tool calls per turn\n",
		"! plan mode: exec and file changes are refused while plan mode is on\n",
	} {
		if !strings.Contains(out.String(), want) {
			t.Fatalf("missing %q in %q", want, out.String())
		}
	}
	if strings.Contains(out.String(), "! exec mode:") {
		t.Fatalf("chat in plan mode claims exec is available: %q", out.String())
	}
}

// Without styling there is no flask: piped output, NO_COLOR, and tests see the
// plain header they always have.
func TestHeader_PlainWithoutStyling(t *testing.T) {
	var out bytes.Buffer
	d := &Display{w: &out}
	d.header(welcomeConfig(t), "/work/repo", "", true)
	if !strings.HasPrefix(out.String(), "re:agent chat · gpt-6-luna (openai, effort low)\n") ||
		strings.ContainsAny(out.String(), "●▟❯\x1b") {
		t.Fatalf("got %q", out.String())
	}
}

func stripANSI(s string) string {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		if s[i] == 0x1b && i+1 < len(s) && s[i+1] == '[' {
			for i += 2; i < len(s) && (s[i] < 0x40 || s[i] > 0x7e); i++ {
			}
			continue
		}
		b.WriteByte(s[i])
	}
	return b.String()
}

func TestChatWelcome_SessionIDAcrossWidths(t *testing.T) {
	id := "0123456789abcdef"
	cfg := welcomeConfig(t)
	for _, mode := range []string{"logo", "plain", "narrow"} {
		t.Run(mode, func(t *testing.T) {
			var out bytes.Buffer
			d := &Display{w: &out, sessionID: id, styled: mode != "plain"}
			if mode == "logo" {
				d.welcome(cfg, "/work/repo", "", welcomeColumns)
			} else {
				d.header(cfg, "/work/repo", "", true)
			}
			text := stripANSI(out.String())
			if !strings.Contains(text, "session ID: "+id) {
				t.Fatalf("missing ID: %q", text)
			}
			if mode == "logo" {
				for _, line := range strings.Split(strings.TrimSuffix(out.String(), "\n"), "\n") {
					if displayWidth(line) >= welcomeColumns {
						t.Fatalf("wrapped: %q", line)
					}
				}
			}
		})
	}
}
