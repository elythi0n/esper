package proto

import (
	"bytes"
	"strings"
	"testing"
	"unicode/utf8"
)

// These mirror the Rust crate's message tests case for case. The two
// implementations must agree on behavior, not merely on bytes: a Go peer that
// truncates a name differently, or intersects capabilities differently, is a
// protocol split that the fixture corpus would not catch.

func helloWithCaps(caps ...string) Hello {
	h := NewHello(RoleHost, bytes.Repeat([]byte{1}, 32), "host", "0.0.0")
	h.Caps = caps
	return h
}

func TestNewHelloSetsTheCurrentProtocolVersion(t *testing.T) {
	if !NewHello(RoleHost, nil, "x", "0.0.0").VersionMatches() {
		t.Error("a freshly built Hello should match our protocol version")
	}
}

func TestVersionMismatchIsDetected(t *testing.T) {
	h := NewHello(RoleController, nil, "x", "0.0.0")
	h.Proto = ProtocolVersion + 1
	if h.VersionMatches() {
		t.Error("a differing protocol version must not match")
	}
}

func TestCapsIntersectAndAreSorted(t *testing.T) {
	a := helloWithCaps("files", "audio", "clipboard")
	b := helloWithCaps("clipboard", "audio", "multimon")
	got := strings.Join(a.CommonCaps(b), ",")
	if got != "audio,clipboard" {
		t.Errorf("CommonCaps = %q, want %q", got, "audio,clipboard")
	}
}

func TestCapsIntersectionIsSymmetric(t *testing.T) {
	a := helloWithCaps("files", "audio")
	b := helloWithCaps("audio", "multimon")
	if x, y := strings.Join(a.CommonCaps(b), ","), strings.Join(b.CommonCaps(a), ","); x != y {
		t.Errorf("intersection is not symmetric: %q vs %q", x, y)
	}
}

func TestUnknownCapIsSimplyAbsentFromTheIntersection(t *testing.T) {
	if got := helloWithCaps("telepathy").CommonCaps(helloWithCaps("audio")); len(got) != 0 {
		t.Errorf("want empty intersection, got %v", got)
	}
}

func TestCapsIntersectionDeduplicates(t *testing.T) {
	a := helloWithCaps("audio", "audio", "files")
	b := helloWithCaps("audio", "files")
	if got := a.CommonCaps(b); len(got) != 2 {
		t.Errorf("want 2 deduplicated caps, got %v", got)
	}
}

func TestLongNamesAreTruncatedNotRejectedAtConstruction(t *testing.T) {
	h := NewHello(RoleHost, nil, strings.Repeat("a", 500), "0.0.0")
	if len(h.Name) != MaxNameLen {
		t.Errorf("name length = %d, want %d", len(h.Name), MaxNameLen)
	}
	if !h.NameIsAcceptable() {
		t.Error("a name we produced ourselves must be acceptable")
	}
}

// The Rust side truncates on a char boundary; Go must do the same, or the two
// produce different names for the same input.
func TestTruncationNeverSplitsAUTF8Character(t *testing.T) {
	for n := 1; n < 80; n++ {
		h := NewHello(RoleHost, nil, strings.Repeat("é", n), "0.0.0")
		if len(h.Name) > MaxNameLen {
			t.Fatalf("n=%d: name is %d bytes, over the cap", n, len(h.Name))
		}
		if !utf8.ValidString(h.Name) {
			t.Fatalf("n=%d: truncation produced invalid UTF-8: %q", n, h.Name)
		}
	}
}

func TestTruncationHandlesFourByteRunes(t *testing.T) {
	// Emoji are four bytes, so 64 is not a multiple of the rune width.
	h := NewHello(RoleHost, nil, strings.Repeat("🔒", 40), "0.0.0")
	if !utf8.ValidString(h.Name) || len(h.Name) > MaxNameLen {
		t.Fatalf("bad truncation: %d bytes, valid=%v", len(h.Name), utf8.ValidString(h.Name))
	}
}

func TestReceivedNameWithControlCharactersIsRejected(t *testing.T) {
	h := NewHello(RoleHost, nil, "ok", "0.0.0")
	h.Name = "evil\a\nname"
	if h.NameIsAcceptable() {
		t.Error("a name containing control characters must be rejected")
	}
}

func TestReceivedOverLongNameIsRejected(t *testing.T) {
	h := NewHello(RoleHost, nil, "ok", "0.0.0")
	h.Name = strings.Repeat("a", MaxNameLen+1)
	if h.NameIsAcceptable() {
		t.Error("an over-long received name must be rejected")
	}
}

// A decoded Hello may carry a device key of any length; it is checked before
// being used for anything.
func TestDeviceKeyLengthIsValidated(t *testing.T) {
	h := NewHello(RoleHost, bytes.Repeat([]byte{9}, 32), "x", "0.0.0")
	if !h.DeviceKeyIsWellFormed() {
		t.Error("a 32-byte key should be well formed")
	}
	for _, n := range []int{0, 1, 31, 33, 64} {
		h.Device = make([]byte, n)
		if h.DeviceKeyIsWellFormed() {
			t.Errorf("a %d-byte key must be rejected", n)
		}
	}
}

// NewHello must always produce a full-length key, even from a short or nil
// input, so a caller cannot accidentally create a malformed Hello.
func TestNewHelloAlwaysProducesAFullLengthKey(t *testing.T) {
	for _, in := range [][]byte{nil, {}, {1, 2, 3}, bytes.Repeat([]byte{4}, 64)} {
		h := NewHello(RoleHost, in, "x", "0.0.0")
		if !h.DeviceKeyIsWellFormed() {
			t.Errorf("input of %d bytes produced a %d-byte key", len(in), len(h.Device))
		}
	}
}

func TestReasonString(t *testing.T) {
	if got := MalformedFrame.String(); got != "malformed frame" {
		t.Errorf("String() = %q", got)
	}
	if got := Reason(9999).String(); got != "unrecognized reason code" {
		t.Errorf("unknown code String() = %q", got)
	}
}

func TestWriteFrameRejectsAnOversizedPayload(t *testing.T) {
	// A name cannot exceed the cap, so build the oversize case directly.
	h := NewHello(RoleHost, nil, "x", strings.Repeat("v", int(MaxFrameLen)+1))
	var buf bytes.Buffer
	if err := WriteFrame(&buf, h); err == nil {
		t.Fatal("an over-cap payload must be rejected")
	}
}

func TestRoleValuesMatchTheWireContract(t *testing.T) {
	// These strings are the wire format and must match the Rust enum's
	// lowercase serde renaming.
	if RoleHost != "host" || RoleController != "controller" {
		t.Errorf("role values drifted: %q / %q", RoleHost, RoleController)
	}
}
