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

// expectedStatus mirrors the Rust side's `expected_status` in
// core/crates/esper-proto/tests/fixtures.rs. Both must describe the same
// value, and the fixture is what makes disagreeing a failure.
func expectedStatus() Report {
	broker := "broker.example.com:7443"
	return Report{Status: &Status{
		Engine:   "0.0.0",
		Capture:  Capability{Available: true, Detail: "x11"},
		Encoder:  Capability{Available: false, Detail: "openh264 starts and cannot encode"},
		Input:    Capability{Available: true, Detail: "x11 xtest: in use"},
		Overlays: []string{"wg0", "tailscale0"},
		MDNS:     true,
		Broker:   &broker,
		Paired:   3,
	}}
}

func TestFixtureStatusDecodesToExpectedValue(t *testing.T) {
	var got Report
	if err := ReadFrame(bytes.NewReader(fixture(t, "status_report.cbor")), &got); err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	want := expectedStatus()
	if got.Status == nil {
		t.Fatalf("decoded %+v, want a status report", got)
	}
	if got.Failed != nil {
		t.Fatalf("a status report decoded as a failure as well: %+v", got)
	}
	g, w := *got.Status, *want.Status
	if g.Engine != w.Engine || g.MDNS != w.MDNS || g.Paired != w.Paired {
		t.Fatalf("status decoded to %+v, want %+v", g, w)
	}
	if g.Capture != w.Capture || g.Encoder != w.Encoder || g.Input != w.Input {
		t.Fatalf("capabilities decoded to %+v, want %+v", g, w)
	}
	if strings.Join(g.Overlays, ",") != strings.Join(w.Overlays, ",") {
		t.Fatalf("overlays decoded to %v, want %v", g.Overlays, w.Overlays)
	}
	if g.Broker == nil || *g.Broker != *w.Broker {
		t.Fatalf("broker decoded to %v, want %v", g.Broker, *w.Broker)
	}
}

// The same contract as TestWireByteCompatibility, for the channel that never
// leaves the machine. It drifts just as silently.
func TestStatusReportByteCompatibility(t *testing.T) {
	want := fixture(t, "status_report.cbor")

	var decoded Report
	if err := ReadFrame(bytes.NewReader(want), &decoded); err != nil {
		t.Fatalf("decode: %v", err)
	}

	var out bytes.Buffer
	if err := WriteFrame(&out, decoded); err != nil {
		t.Fatalf("re-encode: %v", err)
	}
	if !bytes.Equal(out.Bytes(), want) {
		t.Fatalf("re-encoded bytes differ from the fixture\n got: %s\nwant: %s",
			hex.EncodeToString(out.Bytes()), hex.EncodeToString(want))
	}
}

// A command is a bare string on the wire, and must stay one: the Rust enum's
// variants carry nothing, and an externally tagged enum represents those as
// strings rather than maps.
func TestCommandIsAString(t *testing.T) {
	var out bytes.Buffer
	if err := WriteFrame(&out, CommandStatus); err != nil {
		t.Fatalf("encode: %v", err)
	}
	// 4 bytes of length, then a CBOR text string of 6 characters: 0x66.
	body := out.Bytes()[4:]
	if len(body) == 0 || body[0] != 0x66 {
		t.Fatalf("a command encoded to %s, which is not a six-character string",
			hex.EncodeToString(body))
	}
}

// expectedSessions mirrors the Rust side's `expected_sessions`.
func expectedSessions() Report {
	list := []Happening{
		{
			At:     1756700000,
			Peer:   "giraffe tapestry geyser rifle jersey domain razor oboe",
			Key:    "be41344cf00e55422005b232a03d3a0d192df96c55b0fe9ba39f4a0791376a4c",
			Device: "laptop",
			Event:  "session_opened",
			Detail: "session opened over direct, granted view, control",
		},
		{
			At:     1756700042,
			Peer:   "giraffe tapestry geyser rifle jersey domain razor oboe",
			Key:    "be41344cf00e55422005b232a03d3a0d192df96c55b0fe9ba39f4a0791376a4c",
			Device: "",
			Event:  "session_closed",
			Detail: "session ended after 42 seconds, the session ended",
		},
	}
	return Report{Sessions: &list}
}

func TestFixtureSessionsDecodesToExpectedValue(t *testing.T) {
	var got Report
	if err := ReadFrame(bytes.NewReader(fixture(t, "sessions_report.cbor")), &got); err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	if got.Sessions == nil {
		t.Fatalf("decoded %+v, want a list of sessions", got)
	}
	want := *expectedSessions().Sessions
	if len(*got.Sessions) != len(want) {
		t.Fatalf("decoded %d lines, want %d", len(*got.Sessions), len(want))
	}
	for i, g := range *got.Sessions {
		if g != want[i] {
			t.Errorf("line %d decoded to %+v, want %+v", i, g, want[i])
		}
	}
}

func TestSessionsReportByteCompatibility(t *testing.T) {
	want := fixture(t, "sessions_report.cbor")

	var decoded Report
	if err := ReadFrame(bytes.NewReader(want), &decoded); err != nil {
		t.Fatalf("decode: %v", err)
	}

	var out bytes.Buffer
	if err := WriteFrame(&out, decoded); err != nil {
		t.Fatalf("re-encode: %v", err)
	}
	if !bytes.Equal(out.Bytes(), want) {
		t.Fatalf("re-encoded bytes differ from the fixture\n got: %s\nwant: %s",
			hex.EncodeToString(out.Bytes()), hex.EncodeToString(want))
	}
}

// The command carrying a field is a one-key map, not a string — the shape an
// externally tagged enum gives a variant with contents.
func TestSessionsCommandIsAMap(t *testing.T) {
	var out bytes.Buffer
	if err := WriteFrame(&out, AskForSessions(20)); err != nil {
		t.Fatalf("encode: %v", err)
	}
	body := out.Bytes()[4:]
	// 0xa1: a map with one pair.
	if len(body) == 0 || body[0] != 0xa1 {
		t.Fatalf("a sessions command encoded to %s, which is not a one-key map",
			hex.EncodeToString(body))
	}
}
