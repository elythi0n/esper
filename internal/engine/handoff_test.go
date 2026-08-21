package engine

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

/*
A binary beside this one beats one on PATH, and that is the whole rule.

Esper ships three binaries that find each other: the window starts the control
plane, the control plane starts the engine, and `esper broker` hands off to a
third. Every one of them looks *beside itself first* — because the ordinary way
to run a build is from the directory it was built in, and the ordinary way to
have a second copy is to have installed one. Preferring PATH would mean a tree
you just built silently running last month's engine against this month's
protocol, and the failure arrives as a version mismatch with no hint that two
copies exist.

It was at 0%. The test puts a binary in both places and checks which one wins,
which is the only way to see an ordering.
*/
func TestABinaryBesideThisOneBeatsOneOnPath(t *testing.T) {
	self, err := os.Executable()
	if err != nil {
		t.Skipf("this platform will not say where the test binary is: %v", err)
	}
	beside := filepath.Dir(self)

	// A name nothing else could have, so a real engine on this machine cannot
	// be mistaken for the fixture.
	const name = "esper-handoff-fixture"

	onPath := t.TempDir()
	write := func(dir, at string, mode os.FileMode) string {
		t.Helper()
		p := filepath.Join(dir, at)
		if err := os.WriteFile(p, []byte("#!/bin/sh\nexit 0\n"), mode); err != nil {
			t.Fatalf("writing %s: %v", p, err)
		}
		return p
	}
	t.Setenv("PATH", onPath)

	// Nowhere at all: an error that says where it looked, because "not found"
	// with no places named is a message nobody can act on.
	_, err = Locate(name)
	if !errors.Is(err, ErrNotFound) {
		t.Fatalf("a binary that is nowhere gave %v, and callers match on ErrNotFound", err)
	}
	if !strings.Contains(err.Error(), beside) || !strings.Contains(err.Error(), "PATH") {
		t.Errorf("the error does not say where it looked: %v", err)
	}

	// On PATH alone: found there.
	wanted := write(onPath, name+exeSuffix(), 0o755)
	got, err := Locate(name)
	if err != nil {
		t.Fatalf("a binary on PATH was not found: %v", err)
	}
	if got != wanted {
		t.Errorf("found %s, and PATH holds %s", got, wanted)
	}

	// In both places: the one beside this binary wins.
	near := write(beside, name+exeSuffix(), 0o755)
	t.Cleanup(func() { _ = os.Remove(near) })
	got, err = Locate(name)
	if err != nil {
		t.Fatalf("a binary in both places was not found: %v", err)
	}
	if got != near {
		t.Errorf("found %s on PATH while %s sat beside this binary — a build would run an installed copy", got, near)
	}

	// A file beside this one that cannot be executed is not a binary, and the
	// answer falls back to PATH rather than to something that will not run.
	if err := os.Chmod(near, 0o644); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	got, err = Locate(name)
	if err != nil || got != wanted {
		t.Errorf("a file beside this one that nothing can execute was chosen: %s, %v", got, err)
	}
}
