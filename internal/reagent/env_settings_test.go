package reagent

import (
	"strings"
	"testing"
	"time"
)

func TestEnvironmentSettings_DigitsAndFourBooleanSpellingsOnly(t *testing.T) {
	for raw, want := range map[string]int64{"0": 0, "7": 7, "0042": 42, "9223372036854775807": 1<<63 - 1} {
		if got, ok := decimalSetting(raw); !ok || got != want {
			t.Fatalf("%q: got %d %v", raw, got, ok)
		}
	}
	for _, raw := range []string{"", "+1", "-1", " 1", "1 ", "1_000", "0x1", "1e3", "١", "9223372036854775808"} {
		if _, ok := decimalSetting(raw); ok {
			t.Fatalf("%q accepted", raw)
		}
	}
	for raw, want := range map[string]bool{"true": true, "1": true, "false": false, "0": false} {
		if got, ok := booleanSetting(raw); !ok || got != want {
			t.Fatalf("%q: got %v %v", raw, got, ok)
		}
	}
	for _, raw := range []string{"", "T", "t", "TRUE", "True", "F", "f", "FALSE", "False", "yes", "on", " true"} {
		if _, ok := booleanSetting(raw); ok {
			t.Fatalf("%q accepted", raw)
		}
	}
}

func TestEnvironmentSettings_TimeoutsShareTheDigitRule(t *testing.T) {
	for _, raw := range []string{"+30", " 30", "30 ", "0x1e"} {
		t.Setenv("REAGENT_RESPONSE_IDLE_TIMEOUT", raw)
		if _, err := responseTimeout("REAGENT_RESPONSE_IDLE_TIMEOUT", ResponseIdleTimeout); err == nil {
			t.Fatalf("%q accepted", raw)
		}
	}
}

func TestEnvironmentSettings_WorkspaceGitTimeout(t *testing.T) {
	t.Setenv("REAGENT_WORKSPACE_GIT_TIMEOUT", "")
	if got := workspaceGitTimeout(); got != 2*time.Second {
		t.Fatalf("default %s", got)
	}
	for raw, want := range map[string]time.Duration{"1": time.Second, "60": time.Minute, "3600": time.Hour} {
		t.Setenv("REAGENT_WORKSPACE_GIT_TIMEOUT", raw)
		if err := validateHTTPEnvironment(); err != nil {
			t.Fatalf("%q: %v", raw, err)
		}
		if got := workspaceGitTimeout(); got != want {
			t.Fatalf("%q: got %s", raw, got)
		}
	}
	for _, raw := range []string{"0", "3601", "+5", " 5", "1.5", "5s"} {
		t.Setenv("REAGENT_WORKSPACE_GIT_TIMEOUT", raw)
		if err := validateHTTPEnvironment(); err == nil || !strings.Contains(err.Error(), "REAGENT_WORKSPACE_GIT_TIMEOUT") {
			t.Fatalf("%q: %v", raw, err)
		}
	}
}

func TestCatalog_PositionsAreDigitsOnly(t *testing.T) {
	if _, err := selectModel("+1"); err == nil || !strings.Contains(err.Error(), "no model named +1") {
		t.Fatalf("signed position: %v", err)
	}
	if _, err := selectModel("99999999999999999999"); err == nil || !strings.Contains(err.Error(), "no model named") {
		t.Fatalf("overflow position: %v", err)
	}
	if _, err := selectModel("999"); err == nil || !strings.Contains(err.Error(), "there is no model 999") {
		t.Fatalf("out of range: %v", err)
	}
	if info, err := selectModel("1"); err != nil || info.ID != modelCatalog[0].ID {
		t.Fatalf("first position: %v", err)
	}
}
