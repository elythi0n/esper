package proto

import (
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"testing"
)

func TestClassesDeriveFromTheHundredsDigit(t *testing.T) {
	for _, tc := range []struct {
		r    Reason
		want Class
	}{
		{MalformedFrame, ClassProtocol},
		{UnknownPeerKey, ClassAuthentication},
		{PermissionDenied, ClassAuthorization},
		{NoCommonCodec, ClassCapability},
		{AllTiersExhausted, ClassTransport},
	} {
		if got := tc.r.Class(); got != tc.want {
			t.Errorf("Reason(%d).Class() = %q, want %q", tc.r, got, tc.want)
		}
	}
}

// A code from a newer peer must survive for display rather than being dropped.
func TestUnknownCodesArePreservedNotRejected(t *testing.T) {
	future := Reason(999)
	if future.Class() != ClassUnknown {
		t.Errorf("class = %q, want unknown", future.Class())
	}
	if _, ok := future.Describe(); ok {
		t.Error("an unknown code should not claim a description")
	}
	if future != 999 {
		t.Error("the raw code must survive")
	}
}

func TestEveryNamedCodeHasADescription(t *testing.T) {
	for r := range descriptions {
		if s, ok := r.Describe(); !ok || s == "" {
			t.Errorf("code %d has no description", r)
		}
		if r.Class() == ClassUnknown {
			t.Errorf("code %d has no class", r)
		}
	}
}

// TestReasonCodesMatchRust parses the Rust crate's codes.rs and asserts that
// every code it defines exists in Go with the same number, and vice versa.
//
// These are two hand-maintained lists of the same wire contract, which is
// exactly the shape of thing that drifts. A comment saying "keep in sync" is
// not a mechanism; this is.
func TestReasonCodesMatchRust(t *testing.T) {
	path := filepath.Join("..", "..", "core", "crates", "ownview-proto", "src", "codes.rs")
	src, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading %s: %v", path, err)
	}

	// pub const SCREAMING_NAME: Self = Self(123);
	re := regexp.MustCompile(`pub const ([A-Z0-9_]+): Self = Self\((\d+)\);`)
	matches := re.FindAllStringSubmatch(string(src), -1)
	if len(matches) == 0 {
		t.Fatal("parsed no constants from codes.rs — has the format changed?")
	}

	rustCodes := make(map[string]Reason, len(matches))
	for _, m := range matches {
		n, err := strconv.ParseUint(m[2], 10, 16)
		if err != nil {
			t.Fatalf("code %s has a non-uint16 value %q", m[1], m[2])
		}
		rustCodes[m[1]] = Reason(n)
	}

	// Rust SCREAMING_SNAKE -> the Go constant of the same value.
	goByValue := make(map[Reason]bool, len(descriptions))
	for r := range descriptions {
		goByValue[r] = true
	}

	for name, code := range rustCodes {
		if !goByValue[code] {
			t.Errorf("Rust defines %s = %d but Go has no code with that value", name, code)
		}
		if _, ok := code.Describe(); !ok {
			t.Errorf("Rust code %s = %d has no Go description", name, code)
		}
	}

	if len(rustCodes) != len(descriptions) {
		t.Errorf("Rust defines %d codes, Go defines %d — one side has an extra",
			len(rustCodes), len(descriptions))
	}
}

func TestReasonForMapsFramingErrors(t *testing.T) {
	if got := ReasonFor(nil); got != 0 {
		t.Errorf("nil error should map to 0, got %d", got)
	}
	if got := ReasonFor(ErrTooLarge); got != FrameTooLarge {
		t.Errorf("ErrTooLarge -> %d, want %d", got, FrameTooLarge)
	}
	for _, err := range []error{ErrEmpty, ErrTruncated, ErrMalformed} {
		if got := ReasonFor(err); got != MalformedFrame {
			t.Errorf("%v -> %d, want %d", err, got, MalformedFrame)
		}
	}
}
