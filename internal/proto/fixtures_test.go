package proto

import (
	"bytes"
	"encoding/hex"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// fixture reads a committed wire fixture. Fixtures live at the project root so
// both language implementations read the same files.
func fixture(t *testing.T, name string) []byte {
	t.Helper()
	path := filepath.Join("..", "..", "testdata", "fixtures", name)
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading %s: %v", path, err)
	}
	return b
}

func expectedHello() Hello {
	h := NewHello(RoleHost, bytes.Repeat([]byte{0xAB}, 32), "fixture-host", "0.0.0")
	h.Caps = []string{"audio", "clipboard"}
	return h
}

func TestFixtureHelloDecodesToExpectedValue(t *testing.T) {
	var got Hello
	if err := ReadFrame(bytes.NewReader(fixture(t, "hello_host.cbor")), &got); err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	want := expectedHello()
	if got.Proto != want.Proto || got.Engine != want.Engine || got.Name != want.Name ||
		got.Role != want.Role || !bytes.Equal(got.Device, want.Device) ||
		strings.Join(got.Caps, ",") != strings.Join(want.Caps, ",") {
		t.Fatalf("fixture decoded to %+v, want %+v", got, want)
	}
}

// TestWireByteCompatibility is the cross-language contract. The same bytes must
// survive a decode/encode round trip unchanged, which is only true while the Go
// and Rust encoders agree — including on struct field order, since neither
// sorts map keys.
//
// If this fails, do not regenerate the fixture. The protocol has split and one
// of the two encoders is wrong.
func TestWireByteCompatibility(t *testing.T) {
	want := fixture(t, "hello_host.cbor")

	var decoded Hello
	if err := ReadFrame(bytes.NewReader(want), &decoded); err != nil {
		t.Fatalf("decode: %v", err)
	}

	var out bytes.Buffer
	if err := WriteFrame(&out, decoded); err != nil {
		t.Fatalf("re-encode: %v", err)
	}

	if !bytes.Equal(out.Bytes(), want) {
		t.Fatalf("re-encoding the fixture produced different bytes\n got: %s\nwant: %s",
			hex.EncodeToString(out.Bytes()), hex.EncodeToString(want))
	}
}

// TestHelloFieldOrderIsLocked asserts the wire field order explicitly. Field
// order is part of the format because neither implementation sorts map keys, so
// reordering a struct field on either side silently changes the bytes.
func TestHelloFieldOrderIsLocked(t *testing.T) {
	payload := fixture(t, "hello_host.cbor")[4:]
	text := string(payload)
	last := -1
	for _, key := range []string{"proto", "engine", "device", "name", "role", "caps"} {
		at := strings.Index(text, key)
		if at < 0 {
			t.Fatalf("key %q missing from the fixture", key)
		}
		if at < last {
			t.Fatalf("field %q is out of order — the Go and Rust structs must declare fields in the same order", key)
		}
		last = at
	}
}
