package server

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

/*
The third place the engine's printed output is a format.

`esper identity` prints the fingerprint two people read aloud to each other,
and this control plane parses it for the key, the file it lives in and the
words. The day the two disagree about what the fingerprint is, the
disagreement looks like an attack — which is why the words come from the
engine rather than being derived a second time here.

The text below is what the engine prints, captured from a real
`esper-engine identity`.
*/
const engineIdentified = `device fingerprint

    kelp nest jersey hydrant
    chorus fiber office nest

  key   6b8b665f2149958b
  file  /home/somebody/.esper/identity.key
`

// anEngineSaying writes a stand-in `esper-engine` that prints a fixed text.
func anEngineSaying(t *testing.T, out string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "engine")
	script := "#!/bin/sh\ncat <<'SAID'\n" + out + "SAID\n"
	if err := os.WriteFile(path, []byte(script), 0o700); err != nil {
		t.Fatalf("writing a stand-in engine: %v", err)
	}
	return path
}

func TestWhatTheEngineSaysItIsIsWhatIsRead(t *testing.T) {
	s, _, _ := serverThatCanWrite(t, anEngineSaying(t, engineIdentified))

	who, err := s.identity(context.Background())
	if err != nil {
		t.Fatalf("reading the identity the engine printed: %v", err)
	}

	if who.Key != "6b8b665f2149958b" {
		t.Errorf("key %q, and the engine printed 6b8b665f2149958b", who.Key)
	}
	if who.File != "/home/somebody/.esper/identity.key" {
		t.Errorf("file %q", who.File)
	}

	// Eight words in the order they are said, because that order is the check
	// two people are performing and two people reading them in different
	// orders disagree about a machine that is fine.
	want := []string{"kelp", "nest", "jersey", "hydrant", "chorus", "fiber", "office", "nest"}
	if len(who.Words) != len(want) {
		t.Fatalf("read %d words: %v", len(who.Words), who.Words)
	}
	for i := range want {
		if who.Words[i] != want[i] {
			t.Errorf("word %d is %q and the engine said %q", i, who.Words[i], want[i])
		}
	}
}

/*
A repeated word is still two words.

"nest" appears twice in the captured fingerprint, once in each line. A reader
that collected words into a set — or matched them one line at a time and kept
the first — would drop the second and hand back seven words for an eight-word
fingerprint, which is a check that fails against a machine that is fine.
*/
func TestARepeatedWordIsNotCollapsed(t *testing.T) {
	s, _, _ := serverThatCanWrite(t, anEngineSaying(t, engineIdentified))
	who, err := s.identity(context.Background())
	if err != nil {
		t.Fatalf("reading: %v", err)
	}
	nests := 0
	for _, w := range who.Words {
		if w == "nest" {
			nests++
		}
	}
	if nests != 2 {
		t.Errorf("the fingerprint says nest twice and %d came back: %v", nests, who.Words)
	}
}

/*
An engine that printed no fingerprint said something, and it is not a
fingerprint.

The failure to guard against is not an error — it is a *plausible* answer
assembled from nothing, because a screen showing an empty fingerprint beside
the words "read this aloud" is a screen that will have somebody confirm a
machine they have not checked.
*/
func TestAnEngineThatPrintsNoFingerprintIsRefused(t *testing.T) {
	for _, said := range []string{
		"device fingerprint\n\n    kelp nest jersey hydrant\n",
		"esper-engine identity: no identity key here yet\n",
		"",
	} {
		s, _, _ := serverThatCanWrite(t, anEngineSaying(t, said))
		who, err := s.identity(context.Background())
		if err == nil {
			t.Errorf("%q was read as an identity: %+v", strings.TrimSpace(said), who)
		}
	}
}

// With no engine beside it, the control plane says which of the two is missing
// rather than reporting the machine as having no identity.
func TestWithNoEngineTheIdentityIsRefusedAndSaysWhy(t *testing.T) {
	s, _, _ := serverThatCanWrite(t, "")
	who, err := s.identity(context.Background())
	if err == nil {
		t.Fatalf("an identity came back with no engine to ask: %+v", who)
	}
	if !strings.Contains(err.Error(), "engine") {
		t.Errorf("the refusal does not name what is missing: %v", err)
	}
}
