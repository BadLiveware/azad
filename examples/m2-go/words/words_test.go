package words

import (
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestUnique(t *testing.T) {
	got := Unique([]string{"gridfire", "azad", "culture", "azad"})
	want := []string{"azad", "culture", "gridfire"}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Errorf("Unique() mismatch (-want +got):\n%s", diff)
	}
}

func TestCounts(t *testing.T) {
	got := Counts([]string{"azad", "azad", "contact"})
	want := map[string]int{"azad": 2, "contact": 1}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Errorf("Counts() mismatch (-want +got):\n%s", diff)
	}
}
