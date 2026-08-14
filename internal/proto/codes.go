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
	// PromptRefused is its own code rather than a timeout, because the two
	// mean opposite things to whoever is waiting: nobody answered is worth
	// trying again when they are back, and somebody declined is not.
	PromptRefused Reason = 305
	// NobodyToAsk is the fail-closed answer, and one a real deployment meets:
	// a host running as a service has no terminal to ask at, so a device that
	// was never granted unattended cannot open a session there. Its own code
	// because the fix is a specific one — grant that permission — and a device
	// told "permission denied" would go looking at the wrong ones.
	NobodyToAsk Reason = 306
	// HostIsLocked has its own code because it is the one refusal that will
	// fix itself: a device told "permission denied" would go asking for a
	// permission it already holds, and one told this knows to try again when
	// somebody has unlocked the machine.
	HostIsLocked Reason = 307
	// AskedEnough is a limit on questions rather than on permissions. Every
	// ask puts a prompt in front of whoever is at that machine, so a session
	// allowed to ask without end can stop somebody working — and the person
	// answering has no way to say "and stop asking". A device told this has
	// not been refused the permission; it has been told to stop knocking.
	AskedEnough Reason = 308
	// NoAskingHere is a standing decision rather than an answer: a device told
	// this gets the same reply every time, and the only way forward is
	// somebody granting the permission at the machine.
	NoAskingHere Reason = 309
)

// Capability.
const (
	NoCommonCodec    Reason = 400
	NoCaptureBackend Reason = 401
	NoDisplay        Reason = 402
	NoInputBackend   Reason = 403
	// NoScreenLocker is a capability rather than a permission: the device is
	// allowed, the host is willing, and the machine has nothing installed that
	// can lock a screen. There is nothing to retry — a person has to install
	// one.
	NoScreenLocker Reason = 405
	// HostIsBusy is a capability that is held rather than absent, which is why
	// it is here and not among the authorizations: nothing about this device
	// was decided, and the same request a minute later may well be answered.
	//
	// Its own code because every neighboring answer sends somebody somewhere
	// useless — PermissionDenied to permissions they already hold,
	// NoCaptureBackend to a host that can never serve, and saying nothing at
	// all to a machine that looks switched off.
	HostIsBusy Reason = 406
	// IndicatorUnconfirmed is a host that cannot confirm its own session
	// indicator is on screen. A capability rather than a permission: the
	// device is allowed and the machine cannot keep the promise that being
	// watched is visible to whoever is sitting at it.
	IndicatorUnconfirmed Reason = 404
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
	PromptRefused:           "somebody at the host declined this session",
	NobodyToAsk:             "this host has nobody to ask, and this device may not connect unattended",
	HostIsLocked:            "the screen at the host is locked",
	AskedEnough:             "this session has asked for more as often as it may",
	NoAskingHere:            "this host does not let a session ask for more",
	NoCommonCodec:           "no codec in common",
	NoCaptureBackend:        "no screen capture backend available",
	NoDisplay:               "no display to capture",
	NoInputBackend:          "no input injection backend available",
	NoScreenLocker:          "the host has no way to lock its own screen",
	HostIsBusy:              "that machine is already in a session",
	IndicatorUnconfirmed:    "the host cannot show that it is being watched",
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
