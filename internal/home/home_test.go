package home

import (
	"os"
	"path/filepath"
	"testing"
)

// TestTheVariableDecides is the reason this package exists: two engines on one
// machine is how every end-to-end check here arranges a host and a controller,
// and it only works if every part of the product honors the same override.
func TestTheVariableDecides(t *testing.T) {
	t.Setenv(Variable, "/somewhere/of/its/own")
	got, err := Dir()
	if err != nil {
		t.Fatalf("resolving: %v", err)
	}
	if got != "/somewhere/of/its/own" {
		t.Fatalf("%s decides, got %q", Variable, got)
	}
}

// TestAnEmptyVariableIsNoVariable: an exported but empty variable is what a
// shell leaves behind, and taking it literally would put Esper's state at the
// filesystem root.
func TestAnEmptyVariableIsNoVariable(t *testing.T) {
	t.Setenv(Variable, "")
	got, err := Dir()
	if err != nil {
		t.Fatalf("resolving: %v", err)
	}
	dir, err := os.UserHomeDir()
	if err != nil {
		t.Skipf("this machine has no home directory: %v", err)
	}
	if got != filepath.Join(dir, ".esper") {
		t.Fatalf("expected the default beside the home directory, got %q", got)
	}
}

// TestTheDefaultIsBesideTheHomeDirectory pins the other half of the rule.
func TestTheDefaultIsBesideTheHomeDirectory(t *testing.T) {
	somebody := t.TempDir()
	t.Setenv("HOME", somebody)
	t.Setenv(Variable, "")
	got, err := Dir()
	if err != nil {
		t.Fatalf("resolving: %v", err)
	}
	if want := filepath.Join(somebody, ".esper"); got != want {
		t.Fatalf("expected %q, got %q", want, got)
	}
}

// TestNothingIsCreated: a home that does not exist yet is a machine that has
// not been paired, and a token belongs beside a device store that pairing made.
// Resolving a path must not be what brings the directory into being.
func TestNothingIsCreated(t *testing.T) {
	within := filepath.Join(t.TempDir(), "not-yet")
	t.Setenv(Variable, within)
	if _, err := Dir(); err != nil {
		t.Fatalf("resolving: %v", err)
	}
	if _, err := os.Stat(within); !os.IsNotExist(err) {
		t.Fatalf("resolving a path created %s", within)
	}
}
