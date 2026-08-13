package devices

import (
	"bytes"
	"encoding/json"
	"errors"
)

// unmarshalOne reads exactly one JSON value, ignoring fields nothing here holds.
//
// # Why unknown fields are ignored rather than refused
//
// Refusing them was the first instinct and it was wrong. Rust adds fields to a
// device record *without* changing the format version — `last_address`,
// `lock_after` and `groups` all arrived that way, each `#[serde(default)]` and
// omitted when absent, precisely so a file from either build reads the same.
// A reader here that refused an unrecognized field would refuse the whole
// trust store the first time anybody added the next one, and the device list
// going blank is the failure this package exists to prevent.
//
// What that costs is bounded: a record could carry a restriction this build
// cannot see, so what is *shown* may overstate what a device may do. Nothing
// is granted by showing it — the engine holds the whole record and is what
// decides — and the format version is still refused when it moves, which is
// how a change too large to read past announces itself.
func unmarshalOne(b []byte, into any) error {
	decoder := json.NewDecoder(bytes.NewReader(b))
	if err := decoder.Decode(into); err != nil {
		return err
	}
	// Trailing content means two values where one was expected, which is not a
	// store however well the first one parsed. Rust's reader refuses this too.
	if decoder.More() {
		return errors.New("there is more than one value here")
	}
	return nil
}
