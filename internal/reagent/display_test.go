package reagent

import (
	"bytes"
	"testing"
	"time"
)

func TestDisplay_PlainOutputHasNoEscapes(t *testing.T) {
	var b bytes.Buffer
	d := NewDisplay(&b)
	d.summary(RunResult{Status: StatusCompleted, Steps: 1}, time.Second, false)
	if bytes.Contains(b.Bytes(), []byte("\x1b")) {
		t.Fatal(b.String())
	}
}
func TestFormatCount(t *testing.T) {
	if got := formatCount(1200); got != "1.2k" {
		t.Fatal(got)
	}
}
func TestFormatElapsed(t *testing.T) {
	if got := formatElapsed(65 * time.Second); got != "1m05s" {
		t.Fatal(got)
	}
}
