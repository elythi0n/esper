// The control plane's channel to the engine.
//
// Not the wire protocol. These messages never leave the machine: they travel
// over the pipe between esper and esper-engine, in the same length-prefixed
// CBOR framing, because one implementation of framing to get right is better
// than two.
//
// The Rust half is core/crates/esper-proto's local module. **Field order is
// the contract** — both encoders emit struct fields in declaration order — so
// these structs must declare theirs in the same order as the Rust ones, and
// the fixture corpus is what catches it when they do not.
package proto

// Command is what the control plane asks the engine to do.
//
// A string on the wire, because Rust's enum is externally tagged and its
// variants carry nothing. Adding a variant that carries something changes that
// representation to a one-key map, and this type changes shape with it.
type Command string

// CommandStatus asks what the engine is and what this machine can do.
//
// The engine probes rather than reporting what it was built with: an encoder
// that is compiled in and cannot start is the failure this exists to find.
const CommandStatus Command = "status"

// Capability is what one thing on this machine can do.
type Capability struct {
	// Available says whether it works now, established by trying it where
	// trying is cheap.
	Available bool `cbor:"available"`
	// Detail is what was found, or why nothing was. Shown to a person; never
	// parsed.
	Detail string `cbor:"detail"`
}

// Status is everything `esper doctor` needs to say what this machine can do.
//
// Presence is deliberately absent: establishing whether this machine can show
// that it is being watched means starting the indicator, which puts a window
// on somebody's screen and takes a global hotkey. A diagnostic must not
// disturb the machine it is diagnosing.
type Status struct {
	Engine   string     `cbor:"engine"`
	Capture  Capability `cbor:"capture"`
	Encoder  Capability `cbor:"encoder"`
	Input    Capability `cbor:"input"`
	Overlays []string   `cbor:"overlays"`
	MDNS     bool       `cbor:"mdns"`
	// Broker is the broker this machine would use, if one is configured. No
	// default exists anywhere in Esper, so nil is the ordinary answer.
	Broker *string `cbor:"broker"`
	Paired uint32  `cbor:"paired"`
}

// Happening is one line of what has happened on this machine.
//
// A rendering of an audit entry, not the entry itself: the audit vocabulary is
// the engine's and it grows, and mirroring the whole of it here would be two
// enums to keep in step for the sake of one screen.
type Happening struct {
	At   uint64 `cbor:"at"`
	Peer string `cbor:"peer"`
	Key  string `cbor:"key"`
	// Device is what that device is called now, or empty if it is not paired
	// any more — looked up when the line is read, so a device somebody
	// unpaired reads as one rather than as a name that no longer means
	// anything.
	Device string `cbor:"device"`
	Event  string `cbor:"event"`
	Detail string `cbor:"detail"`
	// Direction is "in" for somebody who connected here and "out" for
	// something this machine did to somebody else. Two questions, one list,
	// and an arrow on each line so a person scanning it can tell them apart.
	Direction string `cbor:"direction,omitempty"`
}

// Arrow is the two characters that say which way a happening went.
//
// Inbound is the default for a line with no direction, which is every line
// written before the field existed.
func (h Happening) Arrow() string {
	if h.Direction == "out" {
		return "→"
	}
	return "←"
}

// Failure is the engine saying it could not carry a command out.
//
// A sentence rather than a code: these are read by a person running
// `esper doctor`, and nothing on this side branches on them.
type Failure struct {
	Detail string `cbor:"detail"`
}

// Report is what the engine says back.
//
// Rust's externally tagged enum is a map with exactly one key, so this is a
// struct of pointers with omitempty and exactly one of them set. Treating it
// as a map[string]any would decode anything and tell the caller nothing.
type Report struct {
	Status   *Status      `cbor:"status,omitempty"`
	Sessions *[]Happening `cbor:"sessions,omitempty"`
	Failed   *Failure     `cbor:"failed,omitempty"`
}

// Which reports the engine actually sent, for a caller that must not treat an
// empty report as an answer.
func (r Report) IsEmpty() bool {
	return r.Status == nil && r.Sessions == nil && r.Failed == nil
}

// SessionsCommand asks for the last `most` things that happened here.
//
// A struct because the Rust variant carries a field, which an externally
// tagged enum represents as a one-key map — the same shape as Report.
type SessionsCommand struct {
	Sessions struct {
		Most uint32 `cbor:"most"`
	} `cbor:"sessions"`
}

// AskForSessions builds the command asking for the last `most` lines.
func AskForSessions(most uint32) SessionsCommand {
	var c SessionsCommand
	c.Sessions.Most = most
	return c
}
