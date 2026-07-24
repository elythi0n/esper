package main

import (
	"bufio"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
)

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

// The front door and the engine must know the same words.
//
// `esper connect` works by handing the whole command line to the engine, so
// the list of verbs to hand over is a copy of the engine's own vocabulary —
// and a copy drifts. When the engine grows a verb, this fails until somebody
// adds it here, which is the only moment anybody would think to.
//
// Skipped without a built engine: this reads the engine's own help.
func TestEveryEngineVerbIsReachableThroughTheFrontDoor(t *testing.T) {
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	binary := filepath.Join(root, "build", "esper-engine")
	if _, err := os.Stat(binary); err != nil {
		t.Skip("no built engine at build/esper-engine — run 'make build'")
	}

	out, err := exec.Command(binary, "help").CombinedOutput()
	if err != nil {
		t.Fatalf("esper-engine help: %v", err)
	}

	// Verbs the front door answers itself, or deliberately does not carry.
	ours := map[string]bool{
		"version":  true, // the control plane has its own
		"help":     true, // the control plane has its own
		"serve":    true, // the seam, spoken by the control plane and not a person
		"loopback": true, // a diagnostic of the pipeline, run against the engine
	}

	verb := regexp.MustCompile(`^  ([a-z-]+)\s{2,}`)
	scanner := bufio.NewScanner(strings.NewReader(string(out)))
	seen := 0
	for scanner.Scan() {
		m := verb.FindStringSubmatch(scanner.Text())
		if m == nil {
			continue
		}
		name := m[1]
		if ours[name] {
			continue
		}
		seen++
		if !isEngineCommand(name) {
			t.Errorf("the engine has a %q command and `esper %s` says it is unknown", name, name)
		}
	}
	if seen == 0 {
		t.Fatal("read no verbs out of the engine's help — the parser has drifted, not the lists")
	}
}

// And nothing is claimed that the engine does not have.
func TestTheFrontDoorClaimsNoVerbTheEngineLacks(t *testing.T) {
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	binary := filepath.Join(root, "build", "esper-engine")
	if _, err := os.Stat(binary); err != nil {
		t.Skip("no built engine at build/esper-engine — run 'make build'")
	}
	out, err := exec.Command(binary, "help").CombinedOutput()
	if err != nil {
		t.Fatalf("esper-engine help: %v", err)
	}
	text := string(out)
	for name := range engineCommands {
		if !strings.Contains(text, "  "+name+" ") {
			t.Errorf("`esper %s` hands off to an engine that has no %q command", name, name)
		}
	}
}
