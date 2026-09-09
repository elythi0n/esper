package home

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

/*
The threat model names this adversary, and says the same thing about it as the
code does.

**Why a test and not a review.** `~/.esper` holding the identity key is the
fact that makes a copy of that directory *be* this machine, and it is the kind
of fact that is true, obvious to whoever wrote it, and written down nowhere a
user will meet. It was written down nowhere at all until the sentence `esper
doctor` now prints existed — so the document and the sentence go together, and
this is what keeps them together.

The document is the product's own, in `docs/`. The planning tree's ADRs are not
readable from here and must not be named — see `check-isolation`.
*/
func TestTheThreatModelNamesWhatACopyOfTheStateIs(t *testing.T) {
	said, err := os.ReadFile(filepath.Join("..", "..", "docs", "threat-model.md"))
	if err != nil {
		t.Fatalf("reading the threat model: %v", err)
	}
	// **Whitespace flattened before anything is looked for.** The document is
	// prose and wraps where it wraps: a phrase this cares about will sooner or
	// later straddle a line break, and a test that then reports the claim as
	// missing is a test that fails for a reflow.
	text := strings.Join(strings.Fields(string(said)), " ")

	// The adversary itself, which needs no code execution at all.
	if !strings.Contains(text, "A copy of this machine's state") {
		t.Error("the threat model does not name the adversary that only needs the bytes")
	}
	// What is in the directory, each named — a reader who is told "state" and
	// not "the identity key" has not been told anything they can act on.
	for _, must := range []string{"identity key", "token", "device list", "record"} {
		if !strings.Contains(text, must) {
			t.Errorf("the threat model does not say the state holds the %s", must)
		}
	}
	// And the answer, which is not Esper's to provide and is Esper's to say.
	if !strings.Contains(text, "full-disk encryption") {
		t.Error("the threat model does not name the answer to a stolen disk")
	}

	// **The sentence the product prints is the document's claim.** A document
	// saying `doctor` reports this, and a `doctor` that does not, is the
	// drift this catches — so the words the code produces are checked to be
	// the words the document promises.
	yes, no := true, false
	for _, at := range []AtRest{
		{Encrypted: &yes, How: "dm-crypt: CRYPT-LUKS2-…"},
		{Encrypted: &no},
		{},
	} {
		said := at.Describe()
		if strings.TrimSpace(said) == "" {
			t.Fatal("a state directory produced no sentence at all")
		}
	}
	if !strings.Contains(text, "encrypted volume") {
		t.Error("the threat model does not say what doctor actually reports")
	}
	// The document claims the answer is asked of the kernel rather than
	// guessed from a name. That claim is only true while `isEncrypted` reads
	// the device-mapper UUID, which is what this asserts by asking it about a
	// device whose *name* says nothing.
	if !strings.Contains(text, "device-mapper UUID") {
		t.Error("the threat model does not say how the answer is established")
	}
	if encrypted, _, known := isEncrypted("/dev/null"); !known || encrypted {
		t.Error("a plain device was not classified by asking the kernel")
	}
}
