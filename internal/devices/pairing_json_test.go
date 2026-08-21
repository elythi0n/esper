package devices

import (
	"encoding/json"
	"testing"
	"time"
)

/*
The two shapes a pairing takes in the file, written and read back.

Rust owns this store and Go reads it — except here, where Go writes one too,
and Rust is what has to accept it. The format is externally tagged: a pairing
that lasts is the bare string `"kept"`, and a one-time one is an object with a
single `once` key. Writing the wrong one produces a store the other half
refuses, and the failure arrives as a device list that will not load on a
machine where nothing is wrong.

`UnmarshalJSON` is exercised by the shared fixture. `MarshalJSON` was not
exercised by anything, which means the half that could produce a file nothing
else can read was the half nobody had run.
*/
func TestAPairingSurvivesBeingWrittenAndReadBack(t *testing.T) {
	for _, c := range []struct {
		name  string
		of    Pairing
		shape string
	}{
		{"a pairing that lasts", Pairing{}, `"kept"`},
		{
			"a one-time pairing not yet used",
			Pairing{Once: &Once{Used: false, ExpiresAt: 1700000000}},
			`{"once":{"used":false,"expires_at":1700000000}}`,
		},
		{
			"a one-time pairing already spent",
			Pairing{Once: &Once{Used: true, ExpiresAt: 1700000000}},
			`{"once":{"used":true,"expires_at":1700000000}}`,
		},
	} {
		t.Run(c.name, func(t *testing.T) {
			written, err := json.Marshal(c.of)
			if err != nil {
				t.Fatalf("writing: %v", err)
			}
			if string(written) != c.shape {
				t.Errorf("written as %s, and the store's shape is %s", written, c.shape)
			}

			var read Pairing
			if err := json.Unmarshal(written, &read); err != nil {
				t.Fatalf("reading back what was just written: %v — %s", err, written)
			}
			if (read.Once == nil) != (c.of.Once == nil) {
				t.Fatalf("a %s came back as the other kind", c.name)
			}
			if read.Once != nil && *read.Once != *c.of.Once {
				t.Errorf("read back %+v, wrote %+v", *read.Once, *c.of.Once)
			}
		})
	}
}

/*
What a pairing admits, and when.

This is the rule the window's `once`/`spent` chip is a picture of, and the one
the engine enforces: a one-time pairing is open until it is used, and then
only until it lapses. Both halves of that matter — a pairing that never lapsed
would leave somebody who paired and never connected holding a working key for
ever, which is the same hole the one-session rule exists to close, reached by
not using it.
*/
func TestWhatAPairingAdmits(t *testing.T) {
	now := uint64(time.Now().Unix())
	hour := uint64(3600)

	for _, c := range []struct {
		name  string
		of    Pairing
		at    uint64
		wants bool
	}{
		{"a pairing that lasts, now", Pairing{}, now, true},
		{"a pairing that lasts, years from now", Pairing{}, now + 100*365*24*hour, true},
		{"unused and not yet lapsed", Pairing{Once: &Once{ExpiresAt: now + hour}}, now, true},
		{"unused, a second before it lapses", Pairing{Once: &Once{ExpiresAt: now + 1}}, now, true},
		{"unused, the second it lapses", Pairing{Once: &Once{ExpiresAt: now}}, now, false},
		{"unused and long lapsed", Pairing{Once: &Once{ExpiresAt: now - hour}}, now, false},
		{"used, and not yet lapsed", Pairing{Once: &Once{Used: true, ExpiresAt: now + hour}}, now, false},
		{"used and lapsed", Pairing{Once: &Once{Used: true, ExpiresAt: now - hour}}, now, false},
	} {
		t.Run(c.name, func(t *testing.T) {
			if got := c.of.Admits(c.at); got != c.wants {
				t.Errorf("%s admits %v, want %v", c.name, got, c.wants)
			}
		})
	}
}
