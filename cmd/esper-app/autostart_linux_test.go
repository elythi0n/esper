//go:build linux

package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// TestTheSwitchIsTheFile is the whole design: there is no state here, only a
// file, so the answer cannot fall out of step with what happens at login.
func TestTheSwitchIsTheFile(t *testing.T) {
	config := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", config)
	entry := filepath.Join(config, "autostart", autostartEntry)

	if now := autostartNow(); now.On || !now.Available {
		t.Fatalf("nothing written yet: %+v", now)
	}

	if err := setAutostart(true); err != nil {
		t.Fatalf("turning it on: %v", err)
	}
	if now := autostartNow(); !now.On {
		t.Error("it should be on")
	} else if now.Path != entry {
		t.Errorf("it should say where it is: %q, wanted %q", now.Path, entry)
	}

	// **The entry names this executable absolutely.** A relative name would
	// start whichever esper-app the login shell's PATH found first, which is
	// how somebody spends an afternoon wondering why their build is not the
	// one running.
	written, err := os.ReadFile(entry)
	if err != nil {
		t.Fatalf("reading the entry: %v", err)
	}
	self, err := os.Executable()
	if err != nil {
		t.Fatalf("finding this program: %v", err)
	}
	if !strings.Contains(string(written), "Exec="+self+" %u") {
		t.Errorf("the entry should exec this program by its path and take a link:\n%s", written)
	}

	// Off means gone, not a flag inside a file that stays.
	if err := setAutostart(false); err != nil {
		t.Fatalf("turning it off: %v", err)
	}
	if _, err := os.Stat(entry); !os.IsNotExist(err) {
		t.Error("off should remove the entry")
	}
	if autostartNow().On {
		t.Error("and then it should read as off")
	}

	// Twice off is not an error: somebody may have deleted it themselves,
	// which is exactly the case this design is meant to survive.
	if err := setAutostart(false); err != nil {
		t.Errorf("turning it off again: %v", err)
	}
}
