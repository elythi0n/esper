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

The text below is what the engine answers, captured from a real
`esper-engine identity --json`.

**The object and not the printed report.** The report is written for a person
and is free to change; a control plane that scraped it went on calling the
whole key a fingerprint the day the report started printing both, and every
screen in the window draws an identicon from that value.
*/
const engineIdentified = `{"public_key":"6b8b665f2149958b3b1a4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f",` +
	`"fingerprint":{"hex":"6b8b665f2149958b",` +
	`"words":["kelp","nest","jersey","hydrant","chorus","fiber","office","nest"]},` +
	`"file":"/home/somebody/.esper/identity.key"}
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
	// **Both, and each in its own field.** The short one is what a screen
	// shows and what an identicon is drawn from; the whole one is what a peer
	// pins. A reader that had only one of them would have to guess which.
	if len(who.Key) != 16 {
		t.Errorf("the key field is %d characters, and a fingerprint is 16: %q", len(who.Key), who.Key)
	}
	if len(who.PublicKey) != 64 {
		t.Errorf("the public key is %d characters, and a key is 64: %q", len(who.PublicKey), who.PublicKey)
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
		// An object with everything but the thing a person reads.
		`{"public_key":"6b8b","file":"/tmp/identity.key"}` + "\n",
		// An empty fingerprint, which is the plausible-looking one.
		`{"fingerprint":{"hex":"","words":[]}}` + "\n",
		// The printed report, which is no longer what is asked for — and
		// which an engine from an older build would answer with.
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
