package server

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// anEngineThatDiscovers writes a stand-in engine printing porcelain lines.
//
// A script rather than a mock, for the same reason the write tests use one:
// what is under test is the argv this builds and the bytes it reads back, and
// the only honest way to see both is to be the program it runs.
func anEngineThatDiscovers(t *testing.T, out string) (path, log string) {
	t.Helper()
	dir := t.TempDir()
	path = filepath.Join(dir, "engine")
	log = filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > " + log + "\ncat <<'PORCELAIN'\n" + out + "\nPORCELAIN\n"
	if err := os.WriteFile(path, []byte(script), 0o700); err != nil {
		t.Fatalf("writing the stand-in engine: %v", err)
	}
	return path, log
}

// TestOnlyPairedDevicesAreSeen is the whole meaning of this route.
//
// A pairing candidate is a machine on the network that this one has never
// trusted. Reporting it as "seen" would put a stranger's name on the Devices
// screen — and reporting it *keyed by fingerprint* would put it in the same
// map as the paired ones, where a screen would draw it as reachable.
func TestOnlyPairedDevicesAreSeen(t *testing.T) {
	t.Parallel()
	const mine = "aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44"
	const stranger = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"

	engine, log := anEngineThatDiscovers(t, ""+
		"paired\t"+mine+"\t192.168.1.24:41748\telythion\n"+
		"candidate\t"+stranger+"\t192.168.1.99:7645\tsomebody else")

	s, _, _ := serverThatCanWrite(t, engine)
	seen, err := s.reachable(context.Background())
	if err != nil {
		t.Fatalf("looking: %v", err)
	}
	if len(seen) != 1 {
		t.Fatalf("one paired device should be seen, got %d: %v", len(seen), seen)
	}
	if got := seen[mine].Address; got != "192.168.1.24:41748" {
		t.Errorf("the address should be the advertised one, got %q", got)
	}
	if _, there := seen[stranger]; there {
		t.Error("a pairing candidate is not a device this machine can reach")
	}

	// The argv, because a look at the network must be bounded and must not
	// print for a person.
	args, err := os.ReadFile(log)
	if err != nil {
		t.Fatalf("reading the arguments: %v", err)
	}
	for _, want := range []string{"discover", "--porcelain", "--seconds"} {
		if !strings.Contains(string(args), want) {
			t.Errorf("the engine should be asked with %q, got %q", want, args)
		}
	}
}

// TestANameIsWhateverThePeerCalledIt is why the format is tab separated and
// why only the first three separators separate: a name comes from the other
// machine, and a reader that split on every tab would lose devices whose
// owner put one in the name.
func TestANameIsWhateverThePeerCalledIt(t *testing.T) {
	t.Parallel()
	const fp = "aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44"
	engine, _ := anEngineThatDiscovers(t, "paired\t"+fp+"\t10.0.0.5:7645\telythion\twith a tab in it")

	s, _, _ := serverThatCanWrite(t, engine)
	seen, err := s.reachable(context.Background())
	if err != nil {
		t.Fatalf("looking: %v", err)
	}
	if got := seen[fp].Address; got != "10.0.0.5:7645" {
		t.Errorf("the address should survive an awkward name, got %q", got)
	}
}

// TestNonsenseOnTheNetworkIsIgnored — a line this cannot read is a line
// something else printed, and a route that failed on one would break the
// screen the first time the engine added a word to its output.
func TestNonsenseOnTheNetworkIsIgnored(t *testing.T) {
	t.Parallel()
	engine, _ := anEngineThatDiscovers(t, ""+
		"\n"+
		"something entirely different\n"+
		"paired\t\t10.0.0.5:7645\tno fingerprint at all\n"+
		"paired\tshort\n")

	s, _, _ := serverThatCanWrite(t, engine)
	seen, err := s.reachable(context.Background())
	if err != nil {
		t.Fatalf("looking: %v", err)
	}
	if len(seen) != 0 {
		t.Errorf("nothing readable was said, so nothing should be seen: %v", seen)
	}
}

// TestWithNoEngineNothingIsClaimed. "We could not look" and "nothing is
// reachable" are different answers, and the second is much the stronger.
func TestWithNoEngineNothingIsClaimed(t *testing.T) {
	t.Parallel()
	s, _, _ := serverThatCanWrite(t, "")
	if _, err := s.reachable(context.Background()); err == nil {
		t.Error("with no engine this should refuse rather than report an empty network")
	}
}
