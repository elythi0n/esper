package server

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"

	"github.com/elythi0n/esper/internal/devices"
)

/*
A device as this API reports it, and the two names it adds.

`listed` **embeds** `devices.Device` and adds two fields of its own, so the
answer is one flat object rather than a record inside a wrapper. That is the
same shape the session trail uses on the Rust side — and there it cost every
file transfer ever recorded: an event field and an entry field shared a name,
flattened into one key, and the line stopped reading back.

Go is quieter about it than serde. A field at the outer level simply *wins*: no
error, no duplicate key, and the embedded one vanishes from the output. So a
field added to `devices.Device` called `fingerprint` would silently stop being
reported, and what somebody reads out to confirm a machine would be the store's
value rather than the derived one — or the other way round, which is worse.

This is the check that says so, in the place somebody adding a field will look.
*/
func TestNothingThisApiAddsSharesANameWithTheRecordItWraps(t *testing.T) {
	added := map[string]bool{}
	outer := reflect.TypeOf(listed{})
	for i := range outer.NumField() {
		field := outer.Field(i)
		if field.Anonymous {
			continue
		}
		name := strings.Split(field.Tag.Get("json"), ",")[0]
		if name == "" || name == "-" {
			continue
		}
		added[name] = true
	}
	if len(added) < 2 {
		t.Fatalf("listed adds %d named field(s), which is not this type", len(added))
	}

	inner := reflect.TypeOf(devices.Device{})
	for i := range inner.NumField() {
		name := strings.Split(inner.Field(i).Tag.Get("json"), ",")[0]
		if added[name] {
			t.Errorf(
				"devices.Device has a field called %q and so does the wrapper around it — "+
					"embedded, the outer one wins silently and the record's own value stops being reported",
				name,
			)
		}
	}
}

/*
And what a caller actually receives.

The two derived fields are the point of the type: a fingerprint is what two
people read to each other, and `admits_a_session` is the difference between a
one-time pairing worth using and one worth clearing out. Both are computed
rather than stored, so nothing in the answer can disagree with the file — and
both have to survive being written as JSON, which the reflection above cannot
show on its own.
*/
func TestADeviceIsReportedFlatWithBothDerivedFacts(t *testing.T) {
	_, _, store := serverThatCanWrite(t, "")
	list, err := devices.Load(store)
	if err != nil {
		t.Fatalf("loading the store: %v", err)
	}
	if len(list) == 0 {
		t.Fatal("the store fixture holds no devices, so nothing is being reported")
	}

	device := list[0]
	raw, err := json.Marshal(listed{
		Device:         device,
		Fingerprint:    device.Fingerprint(),
		AdmitsASession: true,
	})
	if err != nil {
		t.Fatalf("writing a device: %v", err)
	}

	var got map[string]json.RawMessage
	if err := json.Unmarshal(raw, &got); err != nil {
		t.Fatalf("reading it back: %v", err)
	}
	// Flat: the record's own fields sit beside the derived ones rather than
	// under a key of their own.
	for _, want := range []string{"name", "permissions", "pairing", "fingerprint", "admits_a_session"} {
		if _, ok := got[want]; !ok {
			t.Errorf("a listed device does not carry %q: %s", want, raw)
		}
	}
	if _, nested := got["Device"]; nested {
		t.Errorf("the record was reported under a key of its own rather than flat: %s", raw)
	}
}
