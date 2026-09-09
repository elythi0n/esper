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
		Engine: "0.0.0",
		// Three shapes on purpose: one that chose between backends, one with
		// nothing to choose between, and one that tried everything and found
		// nothing.
		Capture: Capability{Available: true, Detail: "x11", Attempts: []string{
			"wlr: the Wayland display: Could not find wayland compositor",
			"x11: in use",
		}},
		Encoder: Capability{Available: false, Detail: "openh264 starts and cannot encode"},
		Input: Capability{Available: false, Detail: "nothing — this machine is view only", Attempts: []string{
			"x11 xtest: no input backend: X11 via DISPLAY: Failed to parse value ''",
			"wayland-portal: no input backend: the RemoteDesktop portal refused",
		}},
		Overlays: []string{"wg0", "tailscale0"},
		MDNS:     true,
		Broker:   &broker,
		Paired:   3,
		Platform: Platform{OS: "linux", Display: "wayland"},
	}}
}

// sameCapability compares a capability including everything that was tried.
//
// The attempts are the half a person reads when something is missing, so a
// comparison that skipped them would pass over a decoder that dropped the
// list entirely.
func sameCapability(got, want Capability) bool {
	if got.Available != want.Available || got.Detail != want.Detail {
		return false
	}
	if len(got.Attempts) != len(want.Attempts) {
		return false
	}
	for i := range got.Attempts {
		if got.Attempts[i] != want.Attempts[i] {
			return false
		}
	}
	return true
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
	for _, pair := range []struct {
		name      string
		got, want Capability
	}{
		{"capture", g.Capture, w.Capture},
		{"encoder", g.Encoder, w.Encoder},
		{"input", g.Input, w.Input},
	} {
		if !sameCapability(pair.got, pair.want) {
			t.Fatalf("%s decoded to %+v, want %+v", pair.name, pair.got, pair.want)
		}
	}
	if strings.Join(g.Overlays, ",") != strings.Join(w.Overlays, ",") {
		t.Fatalf("overlays decoded to %v, want %v", g.Overlays, w.Overlays)
	}
	if g.Broker == nil || *g.Broker != *w.Broker {
		t.Fatalf("broker decoded to %v, want %v", g.Broker, *w.Broker)
	}
	// The platform, which is what lets a screen name a remedy rather than only
	// a fault. A missing one reads as "not said", and the screens have a
	// sentence for that — but a *wrong* one sends somebody to the wrong
	// settings pane, so it is checked like everything else on this seam.
	if g.Platform != w.Platform {
		t.Fatalf("platform decoded to %+v, want %+v", g.Platform, w.Platform)
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
			At:        1756700000,
			Peer:      "giraffe tapestry geyser rifle jersey domain razor oboe",
			Key:       "be41344cf00e55422005b232a03d3a0d192df96c55b0fe9ba39f4a0791376a4c",
			Device:    "laptop",
			Event:     "session_opened",
			Detail:    "session opened over direct, granted view, control",
			Direction: "in",
		},
		{
			At:     1756700042,
			Peer:   "giraffe tapestry geyser rifle jersey domain razor oboe",
			Key:    "be41344cf00e55422005b232a03d3a0d192df96c55b0fe9ba39f4a0791376a4c",
			Device: "",
			Event:  "session_closed",
			Detail: "session ended after 42 seconds, the session ended",
			// The other direction, so the fixture covers both halves of a
			// field whose whole job is telling them apart.
			Direction: "out",
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

// expectedAlive mirrors the Rust side's `expected_alive` in
// core/crates/esper-proto/tests/fixtures.rs. Both read the same file.
func expectedAlive() Alive {
	return Alive{
		Since:     1_756_700_000,
		Listening: "0.0.0.0:7645",
		Serving:   "giraffe tapestry geyser rifle jersey domain razor oboe",
		Served:    3,
	}
}

func TestFixtureAliveDecodesToExpectedValue(t *testing.T) {
	var got Report
	if err := ReadFrame(bytes.NewReader(fixture(t, "alive_report.cbor")), &got); err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	if got.Alive == nil {
		t.Fatalf("decoded %+v, want an alive report", got)
	}
	if *got.Alive != expectedAlive() {
		t.Fatalf("alive decoded to %+v, want %+v", *got.Alive, expectedAlive())
	}
	// The same bytes must not also read as anything else: a report is one
	// answer, and a decoder that filled two fields would let a caller branch
	// on whichever it looked at first.
	if got.Status != nil || got.Sessions != nil || got.Failed != nil {
		t.Fatalf("an alive report decoded as something else as well: %+v", got)
	}
}

func TestFixtureAliveReEncodesByteIdentically(t *testing.T) {
	want := fixture(t, "alive_report.cbor")

	var decoded Report
	if err := ReadFrame(bytes.NewReader(want), &decoded); err != nil {
		t.Fatalf("decode: %v", err)
	}
	var out bytes.Buffer
	if err := WriteFrame(&out, decoded); err != nil {
		t.Fatalf("re-encode: %v", err)
	}
	if !bytes.Equal(out.Bytes(), want) {
		t.Fatalf("the encoders have drifted\n got: %s\nwant: %s",
			hex.EncodeToString(out.Bytes()), hex.EncodeToString(want))
	}
}
