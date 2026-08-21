package devices

import (
	"encoding/json"
	"os"
	"testing"
)

/*
The list in this package is the list in the shared file.

`esper-crypto`'s tests say every key in `testdata/fixtures/weak_keys.json` is
one *Rust* refuses. This says the same file is what Go compiled in. Between
them the two halves cannot drift, and neither owns the set alone — which is the
arrangement the wire fixtures and the word list already use, for the same
reason: a list in two places is a list that disagrees with itself.
*/
func TestTheWeakKeysAreTheSharedOnes(t *testing.T) {
	raw, err := os.ReadFile("../../testdata/fixtures/weak_keys.json")
	if err != nil {
		t.Fatalf("the shared weak-key list: %v", err)
	}
	var shared struct {
		Keys []string `json:"keys"`
	}
	if err := json.Unmarshal(raw, &shared); err != nil {
		t.Fatalf("reading it: %v", err)
	}
	// A floor: an empty file agrees with anything.
	if len(shared.Keys) < 8 {
		t.Fatalf("the shared list names %d key(s), and Ed25519 has eight small-order points", len(shared.Keys))
	}
	if len(shared.Keys) != len(weakKeys) {
		t.Fatalf("the shared list has %d keys and this build compiled in %d", len(shared.Keys), len(weakKeys))
	}

	mine := map[string]bool{}
	for _, k := range weakKeys {
		mine[k.String()] = true
	}
	for _, hexKey := range shared.Keys {
		if !mine[hexKey] {
			t.Errorf("the shared list names %s and this build does not refuse it", hexKey)
		}
	}
}

/*
And what the rule does to a store.

The fixture holds three devices: one ordinary, one whose key is not a point at
all, and one of small order. The engine lists two of them — it has the curve
and refuses the weak one — and before this rule Go listed all three, which is a
machine offered on the Devices screen that the engine will not talk to.

The key that is *not a point* is deliberately still listed here: Go cannot tell,
and the engine refuses the session rather than the listing. That difference is
narrow and it is written down rather than left to be discovered.
*/
func TestAStoreWithUnusableKeysListsWhatTheEngineWould(t *testing.T) {
	list, err := Load("../../testdata/fixtures/device_store_unusable.json")
	if err != nil {
		t.Fatalf("loading: %v", err)
	}
	if len(list) != 2 {
		names := make([]string, 0, len(list))
		for _, d := range list {
			names = append(names, d.Name)
		}
		t.Fatalf("listed %d device(s) where the engine lists 2: %v", len(list), names)
	}
	for _, d := range list {
		if !usable(d.PublicKey) {
			t.Errorf("a device nothing can be trusted under was listed: %s", d.Name)
		}
		if d.Name == "a key of small order" {
			t.Error("the small-order key was listed, and the engine refuses it")
		}
	}
}
