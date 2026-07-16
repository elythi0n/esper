package proto

import "errors"

// Reason is a numeric protocol reason code. Codes exist so clients can branch;
// the accompanying string exists so the person reading the UI understands.
// Every failure carries both.
//
//	0xx  Normal         — the session ended rather than failed
//	1xx  Protocol       — version mismatch, malformed frame
//	2xx  Authentication — unknown key, changed key, pairing failed
//	3xx  Authorization  — permission denied, revoked, out of schedule
//	4xx  Capability     — no common codec, no capture backend, no display
//	5xx  Transport      — tiers exhausted, relay full, migration failed
//
// These values are the wire contract and must stay in step with the Rust
// crate's codes.rs. TestReasonCodesMatchRust locks the numbering.
type Reason uint16

// Normal — a session that ended, rather than failed.
//
// Every other class is a failure. Without a class for endings, a session
// closing normally has to pick whichever failure is nearest, and a controller
// cannot tell "the person closed the window" from "the host refused you".
const (
	ClosedByUser     Reason = 1
	SessionComplete  Reason = 2
	HostShuttingDown Reason = 3
	Superseded       Reason = 4
	PeerGone         Reason = 5
	StoppedAtHost    Reason = 6
)

// Protocol.
const (
	ProtocolVersionMismatch Reason = 100
	MalformedFrame          Reason = 101
	FrameTooLarge           Reason = 102
	UnknownMessage          Reason = 103
)

// Authentication.
const (
	UnknownPeerKey     Reason = 200
	PeerKeyChanged     Reason = 201
	PairingFailed      Reason = 202
	PairingExpired     Reason = 203
	PairingRateLimited Reason = 204
)

// Authorization.
const (
	PermissionDenied Reason = 300
	Revoked          Reason = 301
	OutOfSchedule    Reason = 302
	HostDisabled     Reason = 303
	PromptTimeout    Reason = 304
)

// Capability.
const (
	NoCommonCodec    Reason = 400
	NoCaptureBackend Reason = 401
	NoDisplay        Reason = 402
	NoInputBackend   Reason = 403
)

// Transport.
const (
	AllTiersExhausted Reason = 500
	RelayFull         Reason = 501
	MigrationFailed   Reason = 502
	IdleTimeout       Reason = 503
)

// Class is the family a reason code belongs to.
type Class string

// The reason classes. Unknown covers a code from a newer peer that this build
// does not recognize — such codes are preserved and displayed, never rejected.
const (
	// ClassNormal is a session that ended rather than failed.
	ClassNormal         Class = "normal"
	ClassProtocol       Class = "protocol"
	ClassAuthentication Class = "authentication"
	ClassAuthorization  Class = "authorization"
	ClassCapability     Class = "capability"
	ClassTransport      Class = "transport"
	ClassUnknown        Class = "unknown"
)

// IsFailure reports whether this code describes a failure rather than an
// ending.
//
// The distinction a caller actually needs: a viewer that closed cleanly should
// say so and exit, and one that was refused should say why and tell somebody.
// Decided by class rather than by listing codes, so a code from a newer peer is
// classified correctly without this build knowing its name.
func (r Reason) IsFailure() bool {
	return r.Class() != ClassNormal
}

// Class returns the family this code belongs to, derived from its hundreds
// digit.
func (r Reason) Class() Class {
	switch r / 100 {
	case 0:
		return ClassNormal
	case 1:
		return ClassProtocol
	case 2:
		return ClassAuthentication
	case 3:
		return ClassAuthorization
	case 4:
		return ClassCapability
	case 5:
		return ClassTransport
	default:
		return ClassUnknown
	}
}

// descriptions holds the human text for every code this build knows.
var descriptions = map[Reason]string{
	ClosedByUser:            "closed",
	SessionComplete:         "session complete",
	HostShuttingDown:        "the host is shutting down",
	Superseded:              "replaced by a newer session",
	PeerGone:                "the peer stopped responding",
	StoppedAtHost:           "the person at the host ended it",
	ProtocolVersionMismatch: "protocol version mismatch",
	MalformedFrame:          "malformed frame",
	FrameTooLarge:           "frame exceeds the maximum length",
	UnknownMessage:          "unknown message type",
	UnknownPeerKey:          "peer key is not paired with this device",
	PeerKeyChanged:          "peer presented a different key; re-pairing required",
	PairingFailed:           "pairing code did not match",
	PairingExpired:          "pairing code has expired",
	PairingRateLimited:      "too many pairing attempts",
	PermissionDenied:        "permission denied",
	Revoked:                 "this device has been revoked",
	OutOfSchedule:           "outside the permitted schedule",
	HostDisabled:            "this machine is not accepting sessions",
	PromptTimeout:           "nobody responded to the connection prompt",
	NoCommonCodec:           "no codec in common",
	NoCaptureBackend:        "no screen capture backend available",
	NoDisplay:               "no display to capture",
	NoInputBackend:          "no input injection backend available",
	AllTiersExhausted:       "every connection tier failed",
	RelayFull:               "relay is at capacity",
	MigrationFailed:         "connection migration failed",
	IdleTimeout:             "session was idle too long",
}

// Describe returns a short human description, and false for a code this build
// does not know. Callers must render the peer-supplied string when it returns
// false — that is how a newer peer's failures stay legible.
func (r Reason) Describe() (string, bool) {
	s, ok := descriptions[r]
	return s, ok
}

// String implements fmt.Stringer.
func (r Reason) String() string {
	if s, ok := descriptions[r]; ok {
		return s
	}
	return "unrecognized reason code"
}

// ReasonFor maps a framing error to the wire reason code to close with.
func ReasonFor(err error) Reason {
	switch {
	case err == nil:
		return 0
	case errors.Is(err, ErrTooLarge):
		return FrameTooLarge
	case errors.Is(err, ErrEmpty), errors.Is(err, ErrTruncated), errors.Is(err, ErrMalformed):
		return MalformedFrame
	default:
		return MalformedFrame
	}
}
