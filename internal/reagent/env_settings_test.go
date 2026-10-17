package reagent

import (
	"strings"
	"testing"
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
