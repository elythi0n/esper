package devices

import "encoding/hex"

// Keys no device may be trusted under.
//
// # Why this list exists in Go at all
//
// Rust refuses a key of small order when it loads the store: anybody can
// produce a valid-looking signature under one, so a device filed with such a
// key is a device the engine will never talk to. Go reads the same store — the
// device list is served from the file directly, because a machine with nothing
// connected has no engine and that is exactly when somebody opens the list.
//
// Without this, the two halves disagreed about what a store contains: the
// engine dropped the entry and this side listed it, so the Devices screen
// offered a machine that could not answer. The loader's own comment already
// promised they matched.
//
// # Why a list rather than a curve check
//
// Ed25519's small-order points are eight fixed byte strings. Checking
// membership needs no field arithmetic and no dependency, which matters in a
// tree that has kept its dependencies countable. The set is not this file's to
// decide: it lives in `testdata/fixtures/weak_keys.json`, `esper-crypto`'s
// tests assert every entry is one *it* refuses, and `weak_test.go` asserts this
// list is that file. Neither side can drift without a test saying so.
var weakKeys = [...]Key{
	mustKey("0100000000000000000000000000000000000000000000000000000000000000"),
	mustKey("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
	mustKey("0000000000000000000000000000000000000000000000000000000000000080"),
	mustKey("0000000000000000000000000000000000000000000000000000000000000000"),
	mustKey("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a"),
	mustKey("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa"),
	mustKey("26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05"),
	mustKey("26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85"),
}

// usable says whether a device may be trusted under this key.
//
// The same question `Identity::public_key_is_usable` answers on the other side,
// for the part of it Go can answer: a key of small order is refused. A key that
// is not a point at all is left to the engine, which has the curve to say so —
// and which refuses the session rather than the listing.
func usable(key Key) bool {
	for _, weak := range weakKeys {
		if key == weak {
			return false
		}
	}
	return true
}

// mustKey parses a compile-time constant, and panics on one that is wrong.
//
// Only ever called on the literals above, which are checked against the shared
// fixture by the tests: a typo in one is a program that will not start rather
// than a device silently trusted.
func mustKey(hexKey string) Key {
	var key Key
	raw, err := hex.DecodeString(hexKey)
	if err != nil || len(raw) != KeyLen {
		panic("a weak-key constant that is not a 32-byte hex string: " + hexKey)
	}
	copy(key[:], raw)
	return key
}
