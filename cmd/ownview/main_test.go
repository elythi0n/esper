package main

import "testing"

func TestRunExitCodes(t *testing.T) {
	for _, tc := range []struct {
		name string
		args []string
		want int
	}{
		{"bare prints help", nil, 0},
		{"help", []string{"help"}, 0},
		{"help flag", []string{"--help"}, 0},
		{"version", []string{"version"}, 0},
		{"version flag", []string{"-v"}, 0},
		{"version json", []string{"version", "--json"}, 0},
		{"unknown command", []string{"nonsense"}, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := run(tc.args); got != tc.want {
				t.Errorf("run(%v) = %d, want %d", tc.args, got, tc.want)
			}
		})
	}
}

func TestDisplayOrDash(t *testing.T) {
	if got := displayOrDash(""); got != "—" {
		t.Errorf("empty should render as an em dash, got %q", got)
	}
	if got := displayOrDash("2026-09-01T00:00:00Z"); got != "2026-09-01T00:00:00Z" {
		t.Errorf("a stamped value should pass through, got %q", got)
	}
}
