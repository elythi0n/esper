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
	Status *Status  `cbor:"status,omitempty"`
	Failed *Failure `cbor:"failed,omitempty"`
}

// Which reports the engine actually sent, for a caller that must not treat an
// empty report as an answer.
func (r Report) IsEmpty() bool { return r.Status == nil && r.Failed == nil }
