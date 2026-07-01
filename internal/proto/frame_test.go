package proto

import (
	"bytes"
	"encoding/binary"
	"errors"
	"strings"
	"testing"
)

func sampleHello() Hello {
	return NewHello(RoleHost, bytes.Repeat([]byte{7}, 32), "test-host", "0.0.0")
}

func TestRoundTrip(t *testing.T) {
	var buf bytes.Buffer
	if err := WriteFrame(&buf, sampleHello()); err != nil {
		t.Fatalf("write: %v", err)
	}
	var got Hello
	if err := ReadFrame(&buf, &got); err != nil {
		t.Fatalf("read: %v", err)
	}
	if got.Name != "test-host" || got.Proto != ProtocolVersion {
		t.Fatalf("round trip lost data: %+v", got)
	}
}

func TestReaderIsLeftAtTheNextFrame(t *testing.T) {
	var buf bytes.Buffer
	for range 2 {
		if err := WriteFrame(&buf, sampleHello()); err != nil {
			t.Fatalf("write: %v", err)
		}
	}
	r := bytes.NewReader(buf.Bytes())
	var a, b Hello
	if err := ReadFrame(r, &a); err != nil {
		t.Fatalf("first: %v", err)
	}
	if err := ReadFrame(r, &b); err != nil {
		t.Fatalf("second: %v", err)
	}
	if r.Len() != 0 {
		t.Fatalf("%d bytes left over; frames were not consumed exactly", r.Len())
	}
}

// A peer claiming 4 GiB must cost us a rejected header, not four gigabytes.
func TestOversizedLengthIsRejectedWithoutAllocating(t *testing.T) {
	var header [4]byte
	binary.BigEndian.PutUint32(header[:], ^uint32(0))
	var h Hello
	err := ReadFrame(bytes.NewReader(header[:]), &h)
	if !errors.Is(err, ErrTooLarge) {
		t.Fatalf("want ErrTooLarge, got %v", err)
	}
	if ReasonFor(err) != FrameTooLarge {
		t.Fatalf("want reason %d, got %d", FrameTooLarge, ReasonFor(err))
	}
}

// Boundary: MaxFrameLen is allowed (and then truncates), MaxFrameLen+1 is not.
func TestLengthCapBoundary(t *testing.T) {
	var atCap, overCap [4]byte
	binary.BigEndian.PutUint32(atCap[:], MaxFrameLen)
	binary.BigEndian.PutUint32(overCap[:], MaxFrameLen+1)

	var h Hello
	if err := ReadFrame(bytes.NewReader(atCap[:]), &h); !errors.Is(err, ErrTruncated) {
		t.Fatalf("at cap: want ErrTruncated, got %v", err)
	}
	if err := ReadFrame(bytes.NewReader(overCap[:]), &h); !errors.Is(err, ErrTooLarge) {
		t.Fatalf("over cap: want ErrTooLarge, got %v", err)
	}
}

func TestZeroLengthIsRejected(t *testing.T) {
	header := make([]byte, 4)
	var h Hello
	if err := ReadFrame(bytes.NewReader(header), &h); !errors.Is(err, ErrEmpty) {
		t.Fatalf("want ErrEmpty, got %v", err)
	}
}

func TestTruncatedStreamReportsTheShortfall(t *testing.T) {
	var buf bytes.Buffer
	if err := WriteFrame(&buf, sampleHello()); err != nil {
		t.Fatalf("write: %v", err)
	}
	short := buf.Bytes()[:buf.Len()-3]
	var h Hello
	err := ReadFrame(bytes.NewReader(short), &h)
	if !errors.Is(err, ErrTruncated) {
		t.Fatalf("want ErrTruncated, got %v", err)
	}
	if !strings.Contains(err.Error(), "wanted") {
		t.Fatalf("error should name the shortfall, got %q", err)
	}
}

func TestEmptyStreamIsTruncatedNotAHang(t *testing.T) {
	var h Hello
	if err := ReadFrame(bytes.NewReader(nil), &h); !errors.Is(err, ErrTruncated) {
		t.Fatalf("want ErrTruncated, got %v", err)
	}
}

func TestGarbagePayloadIsRejectedCleanly(t *testing.T) {
	payload := []byte("this is not cbor at all")
	buf := make([]byte, 4, 4+len(payload))
	binary.BigEndian.PutUint32(buf, uint32(len(payload)))
	buf = append(buf, payload...)

	var h Hello
	err := ReadFrame(bytes.NewReader(buf), &h)
	if !errors.Is(err, ErrMalformed) {
		t.Fatalf("want ErrMalformed, got %v", err)
	}
	if ReasonFor(err) != MalformedFrame {
		t.Fatalf("want reason %d, got %d", MalformedFrame, ReasonFor(err))
	}
}

// Every prefix of a valid frame must error, never panic. The always-on cheap
// version of the fuzz target.
func TestNoPrefixOfAValidFramePanics(t *testing.T) {
	var buf bytes.Buffer
	if err := WriteFrame(&buf, sampleHello()); err != nil {
		t.Fatalf("write: %v", err)
	}
	full := buf.Bytes()
	for n := range len(full) {
		var h Hello
		_ = ReadFrame(bytes.NewReader(full[:n]), &h)
	}
}

// Every single-byte corruption must error, never panic.
func TestNoSingleByteCorruptionPanics(t *testing.T) {
	var buf bytes.Buffer
	if err := WriteFrame(&buf, sampleHello()); err != nil {
		t.Fatalf("write: %v", err)
	}
	full := buf.Bytes()
	for i := range full {
		bad := bytes.Clone(full)
		bad[i] += 0x5A
		var h Hello
		_ = ReadFrame(bytes.NewReader(bad), &h)
	}
}

// A deeply nested payload must be rejected by the depth cap rather than
// exhausting the stack.
func TestDeeplyNestedPayloadIsRejected(t *testing.T) {
	// 64 nested single-element arrays: 0x81 repeated, then a zero.
	payload := bytes.Repeat([]byte{0x81}, 64)
	payload = append(payload, 0x00)
	buf := make([]byte, 4, 4+len(payload))
	binary.BigEndian.PutUint32(buf, uint32(len(payload)))
	buf = append(buf, payload...)

	var v any
	if err := ReadFrame(bytes.NewReader(buf), &v); !errors.Is(err, ErrMalformed) {
		t.Fatalf("want ErrMalformed from the nesting cap, got %v", err)
	}
}

func FuzzReadFrame(f *testing.F) {
	var seed bytes.Buffer
	_ = WriteFrame(&seed, sampleHello())
	f.Add(seed.Bytes())
	f.Add([]byte{0, 0, 0, 0})
	f.Add([]byte{255, 255, 255, 255})
	f.Add([]byte{})

	f.Fuzz(func(_ *testing.T, data []byte) {
		var h Hello
		// The contract is only that this returns rather than panics.
		_ = ReadFrame(bytes.NewReader(data), &h)
	})
}
