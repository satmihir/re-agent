package reagent

import (
	"strings"
	"testing"
)

func TestSelectModel_ByPositionOrName(t *testing.T) {
	first, err := selectModel("1")
	if err != nil || first.ID != modelCatalog[0].ID {
		t.Fatalf("got %+v %v", first, err)
	}
	byName, err := selectModel("claude-sonnet-5")
	if err != nil || byName.Provider != anthropicName {
		t.Fatalf("got %+v %v", byName, err)
	}
	byPosition, err := selectModel("3")
	if err != nil || byPosition.ID != "gpt-6-luna" {
		t.Fatalf("position 3 got %+v %v", byPosition, err)
	}
	for _, choice := range []string{"0", "99", "-1", "gpt-nonexistent", ""} {
		if _, err := selectModel(choice); err == nil {
			t.Fatalf("%q was accepted", choice)
		}
	}
}

func TestSelectEffort_AgainstTheModelsOwnVocabulary(t *testing.T) {
	sonnet, _ := findModel("claude-sonnet-5")
	haiku, _ := findModel("claude-haiku-4-5")
	luna, _ := findModel("gpt-5.6-luna")

	if got, err := selectEffort(sonnet, "2"); err != nil || got != "medium" {
		t.Fatalf("got %q %v", got, err)
	}
	if got, err := selectEffort(sonnet, "max"); err != nil || got != "max" {
		t.Fatalf("got %q %v", got, err)
	}
	// "none" is an OpenAI value; Sonnet does not take it.
	if _, err := selectEffort(sonnet, "none"); err == nil {
		t.Fatal("sonnet accepted none")
	}
	if got, err := selectEffort(luna, "none"); err != nil || got != "none" {
		t.Fatalf("got %q %v", got, err)
	}
	// A model that rejects the parameter outright says so rather than listing.
	_, err := selectEffort(haiku, "low")
	if err == nil || !strings.Contains(err.Error(), "takes no reasoning effort") {
		t.Fatalf("got %v", err)
	}
	if _, err := selectEffort(sonnet, "9"); err == nil {
		t.Fatal("an out-of-range position was accepted")
	}
}

func TestRenderModels_MarksCurrentAndUnusable(t *testing.T) {
	listing := renderModels("gpt-5.6-luna", map[string]bool{openaiName: true})

	for _, want := range []string{
		"1  gpt-5.6-luna      openai     current",
		"3  gpt-6-luna        openai",
		"4  claude-haiku-4-5  anthropic  no key",
		"models marked no key need ANTHROPIC_API_KEY",
		"a model change starts a fresh session",
	} {
		if !strings.Contains(listing, want) {
			t.Fatalf("listing lacks %q:\n%s", want, listing)
		}
	}
	// No row may trail whitespace where its status column is empty.
	for _, line := range strings.Split(listing, "\n") {
		if line != strings.TrimRight(line, " ") {
			t.Fatalf("line has trailing spaces: %q", line)
		}
	}

	both := renderModels("gpt-5.6-luna", map[string]bool{openaiName: true, anthropicName: true})
	if strings.Contains(both, "no key") {
		t.Fatalf("a usable provider was marked:\n%s", both)
	}
}

func TestRenderEfforts_ListsOrExplainsAbsence(t *testing.T) {
	sonnet, _ := findModel("claude-sonnet-5")
	if got := renderEfforts(sonnet, "high"); !strings.Contains(got, "3  high  [current]") {
		t.Fatalf("got:\n%s", got)
	}
	haiku, _ := findModel("claude-haiku-4-5")
	if got := renderEfforts(haiku, ""); got != "claude-haiku-4-5 takes no reasoning effort setting" {
		t.Fatalf("got %q", got)
	}
}

// The catalog's effort lists are the reason /effort can refuse locally, so
// they are pinned rather than left to drift.
func TestModelCatalog_EffortSupport(t *testing.T) {
	for _, c := range []struct {
		id      string
		efforts string
	}{
		{"gpt-5.6-luna", "none,low,medium,high,xhigh,max"},
		{"gpt-5.6-terra", "none,low,medium,high,xhigh,max"},
		{"gpt-6-luna", "none,low,medium,high,xhigh,max"},
		{"claude-haiku-4-5", ""},
		{"claude-sonnet-5", "low,medium,high,xhigh,max"},
	} {
		info, found := findModel(c.id)
		if !found {
			t.Fatalf("%s is not in the catalog", c.id)
		}
		if got := strings.Join(info.Efforts, ","); got != c.efforts {
			t.Fatalf("%s accepts %q, want %q", c.id, got, c.efforts)
		}
		// A model's own default must be one it accepts, or unset.
		if info.Effort != "" && !info.accepts(info.Effort) {
			t.Fatalf("%s defaults to %q, which it does not accept", c.id, info.Effort)
		}
	}
}
