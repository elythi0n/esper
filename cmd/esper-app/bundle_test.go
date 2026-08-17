package main

import "testing"

// TestTheBundleHasAFingerprint, and it is stable.
//
// Stability is the whole value: two runs of the same binary must agree, or
// the number is noise. `fs.WalkDir` is lexical *within* a directory and this
// walks several, which is why the names are sorted before hashing.
func TestTheBundleHasAFingerprint(t *testing.T) {
	t.Parallel()
	first, err := bundleFingerprint()
	if err != nil {
		t.Fatalf("fingerprinting: %v", err)
	}
	if len(first) != 16 {
		t.Errorf("a fingerprint should be 16 hex characters, got %q", first)
	}
	again, err := bundleFingerprint()
	if err != nil {
		t.Fatalf("fingerprinting again: %v", err)
	}
	if first != again {
		t.Errorf("two runs disagreed: %q then %q", first, again)
	}
	if bundleFiles() < 1 {
		t.Error("the bundle should hold at least the placeholder index.html")
	}
}
