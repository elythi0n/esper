// Package proto is the Go half of the Esper wire protocol: the same
// length-prefixed CBOR framing, messages and reason codes that
// core/crates/esper-proto implements in Rust.
//
// Two independent implementations of one protocol drift silently, so they are
// held together by a shared fixture corpus: every message in
// testdata/fixtures is decoded and re-encoded by both languages and must
// produce byte-identical output. See frame_test.go and the Rust crate's
// corresponding test.
//
// The security posture matches the Rust side. This is the first parser
// attacker-controlled bytes reach, so the length is validated before anything
// is allocated, and every failure is a typed error carrying a wire reason code
// rather than a panic.
package proto

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"

	"github.com/fxamacker/cbor/v2"
)

// ProtocolVersion is the wire protocol version. It must match exactly between
// peers; a mismatch is a clean close naming both versions, never a negotiation.
const ProtocolVersion uint16 = 1

// ALPN identifies the QUIC connection.
const ALPN = "esper/1"

// MaxFrameLen is the maximum control frame length. Anything larger is a bulk
// stream by definition, so this is a protocol invariant rather than a tuning
// knob.
const MaxFrameLen uint32 = 1024 * 1024

// lenPrefix is the size of the big-endian uint32 length header.
const lenPrefix = 4

// Framing errors. Callers match with errors.Is and map to a Reason via
// ReasonFor.
var (
	// ErrTooLarge means the declared length exceeds MaxFrameLen. Returned
	// before any payload buffer is allocated.
	ErrTooLarge = errors.New("frame exceeds maximum length")
	// ErrEmpty means a zero-length frame, which carries no CBOR and is always
	// a protocol error.
	ErrEmpty = errors.New("frame length is zero")
	// ErrTruncated means the stream ended mid-frame.
	ErrTruncated = errors.New("stream ended mid-frame")
	// ErrMalformed means the payload was not valid CBOR of the expected shape.
	ErrMalformed = errors.New("malformed CBOR payload")
)

// decMode rejects the ambiguous and hostile corners of CBOR rather than
// accepting them. In particular it caps nesting depth, so a deeply nested
// payload cannot exhaust the stack, and it refuses duplicate map keys, which
// are a classic way to make two parsers disagree about the same bytes.
var decMode = func() cbor.DecMode {
	m, err := cbor.DecOptions{
		MaxNestedLevels:  16,
		DupMapKey:        cbor.DupMapKeyEnforcedAPF,
		IndefLength:      cbor.IndefLengthForbidden,
		UTF8:             cbor.UTF8RejectInvalid,
		MaxArrayElements: 8192,
		MaxMapPairs:      8192,
	}.DecMode()
	if err != nil {
		panic("proto: invalid CBOR decode options: " + err.Error())
	}
	return m
}()

// encMode produces deterministic CBOR that is byte-identical to what the Rust
// crate's ciborium encoder produces for the same value.
//
// It starts from canonical options — shortest-form integers, definite lengths —
// but sets Sort to SortNone. That is deliberate and load-bearing. RFC canonical
// CBOR sorts map keys by length then bytes; ciborium emits struct fields in
// declaration order and has no canonical mode. Sorting here would make the two
// implementations produce different bytes for the same message, which would
// defeat the whole point of the shared fixture corpus.
//
// The encoding is still fully deterministic — the same value always produces
// the same bytes — which is the property that actually matters. What it depends
// on is the Go and Rust structs declaring their fields in the same order, and
// TestWireByteCompatibility locks that down so a reordering on either side
// fails loudly instead of silently splitting the protocol.
var encMode = func() cbor.EncMode {
	opts := cbor.CanonicalEncOptions()
	opts.Sort = cbor.SortNone
	m, err := opts.EncMode()
	if err != nil {
		panic("proto: invalid CBOR encode options: " + err.Error())
	}
	return m
}()

// ReadFrame reads one frame from r and decodes it into v, which must be a
// pointer. It reads exactly the declared number of bytes and no more, leaving r
// positioned at the start of the next frame.
func ReadFrame(r io.Reader, v any) error {
	var header [lenPrefix]byte
	if err := readExact(r, header[:]); err != nil {
		return err
	}
	length := binary.BigEndian.Uint32(header[:])

	// Validate before allocating. This ordering is the point of the function:
	// a peer claiming 4 GiB must cost a rejected header, not four gigabytes.
	switch {
	case length == 0:
		return ErrEmpty
	case length > MaxFrameLen:
		return fmt.Errorf("%w: %d > %d", ErrTooLarge, length, MaxFrameLen)
	}

	payload := make([]byte, length)
	if err := readExact(r, payload); err != nil {
		return err
	}
	if err := decMode.Unmarshal(payload, v); err != nil {
		return fmt.Errorf("%w: %v", ErrMalformed, err)
	}
	return nil
}

// WriteFrame encodes v as canonical CBOR and writes it to w with its length
// prefix.
func WriteFrame(w io.Writer, v any) error {
	payload, err := encMode.Marshal(v)
	if err != nil {
		return fmt.Errorf("proto: encode: %w", err)
	}
	switch {
	case len(payload) == 0:
		return ErrEmpty
	case uint64(len(payload)) > uint64(MaxFrameLen):
		return fmt.Errorf("%w: %d > %d", ErrTooLarge, len(payload), MaxFrameLen)
	}

	var header [lenPrefix]byte
	binary.BigEndian.PutUint32(header[:], uint32(len(payload)))
	if _, err := w.Write(header[:]); err != nil {
		return fmt.Errorf("proto: write header: %w", err)
	}
	if _, err := w.Write(payload); err != nil {
		return fmt.Errorf("proto: write payload: %w", err)
	}
	return nil
}

// readExact fills buf, translating a short read into ErrTruncated with the
// shortfall attached. io.ReadFull collapses that into an opaque
// ErrUnexpectedEOF; knowing how much was missing is the difference between a
// useful log line and a mystery.
func readExact(r io.Reader, buf []byte) error {
	n, err := io.ReadFull(r, buf)
	switch {
	case err == nil:
		return nil
	case errors.Is(err, io.EOF), errors.Is(err, io.ErrUnexpectedEOF):
		return fmt.Errorf("%w: wanted %d bytes, got %d", ErrTruncated, len(buf), n)
	default:
		return fmt.Errorf("proto: read: %w", err)
	}
}
