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

// CommandAlive asks a running host whether it is working.
//
// Deliberately not CommandStatus, which probes the encoder and the capture
// backend: asked on a timer that would start and stop an encoder every few
// seconds for as long as the machine runs. A supervisor watching a host for
// weeks needs a question that costs nothing to answer.
const CommandAlive Command = "alive"

// Alive is what a host is doing right now.
//
// An answer proves the engine's runtime is scheduling work and its state is
// readable — the difference a supervisor cannot otherwise see, because a
// wedged process and a working one look identical from outside. It does not
// prove the screen is being captured; see the Rust doc comment for why
// claiming that here would be a health check that lies.
type Alive struct {
	Since     uint64 `cbor:"since" json:"since"`
	Listening string `cbor:"listening" json:"listening"`
	// Serving is who is connected right now, in the word form a person can
	// compare against a device list, or empty when nobody is.
	Serving string `cbor:"serving" json:"serving"`
	// Served is how many sessions this host has served since it started. A
	// count that never moves is an idle host; one that climbs is a used one.
	Served uint32 `cbor:"served" json:"served"`
}

// Capability is what one thing on this machine can do.
type Capability struct {
	// Available says whether it works now, established by trying it where
	// trying is cheap.
	Available bool `cbor:"available" json:"available"`
	// Detail is what was found, or why nothing was. Shown to a person; never
	// parsed.
	Detail string `cbor:"detail" json:"detail"`
}

// Status is everything `esper doctor` needs to say what this machine can do.
//
// Presence is deliberately absent: establishing whether this machine can show
// that it is being watched means starting the indicator, which puts a window
// on somebody's screen and takes a global hotkey. A diagnostic must not
// disturb the machine it is diagnosing.
// The `json` tags mirror the `cbor` ones on purpose: these names are what the
// local API answers with, so a dashboard, an MCP adapter and a packet capture
// of the seam all read the same words. Without them Go serves its *field*
// names — `{"At":…,"Peer":…}` — which makes an internal detail into a public
// contract, and a rename into a breaking change nobody meant to make.
type Status struct {
	Engine   string     `cbor:"engine" json:"engine"`
	Capture  Capability `cbor:"capture" json:"capture"`
	Encoder  Capability `cbor:"encoder" json:"encoder"`
	Input    Capability `cbor:"input" json:"input"`
	Overlays []string   `cbor:"overlays" json:"overlays"`
	MDNS     bool       `cbor:"mdns" json:"mdns"`
	// Broker is the broker this machine would use, if one is configured. No
	// default exists anywhere in Esper, so nil is the ordinary answer.
	Broker *string `cbor:"broker" json:"broker"`
	Paired uint32  `cbor:"paired" json:"paired"`
}

// Happening is one line of what has happened on this machine.
//
// A rendering of an audit entry, not the entry itself: the audit vocabulary is
// the engine's and it grows, and mirroring the whole of it here would be two
// enums to keep in step for the sake of one screen.
type Happening struct {
	At   uint64 `cbor:"at" json:"at"`
	Peer string `cbor:"peer" json:"peer"`
	Key  string `cbor:"key" json:"key"`
	// Device is what that device is called now, or empty if it is not paired
	// any more — looked up when the line is read, so a device somebody
	// unpaired reads as one rather than as a name that no longer means
	// anything.
	Device string `cbor:"device" json:"device"`
	Event  string `cbor:"event" json:"event"`
	Detail string `cbor:"detail" json:"detail"`
	// Direction is "in" for somebody who connected here and "out" for
	// something this machine did to somebody else. Two questions, one list,
	// and an arrow on each line so a person scanning it can tell them apart.
	Direction string `cbor:"direction,omitempty" json:"direction,omitempty"`
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
	Detail string `cbor:"detail" json:"detail"`
}

// Report is what the engine says back.
//
// Rust's externally tagged enum is a map with exactly one key, so this is a
// struct of pointers with omitempty and exactly one of them set. Treating it
// as a map[string]any would decode anything and tell the caller nothing.
type Report struct {
	Status   *Status      `cbor:"status,omitempty" json:"status,omitempty"`
	Sessions *[]Happening `cbor:"sessions,omitempty" json:"sessions,omitempty"`
	Alive    *Alive       `cbor:"alive,omitempty" json:"alive,omitempty"`
	Failed   *Failure     `cbor:"failed,omitempty" json:"failed,omitempty"`
}

// Which reports the engine actually sent, for a caller that must not treat an
// empty report as an answer.
func (r Report) IsEmpty() bool {
	return r.Status == nil && r.Sessions == nil && r.Alive == nil && r.Failed == nil
}

// SessionsCommand asks for the last `most` things that happened here.
//
// A struct because the Rust variant carries a field, which an externally
// tagged enum represents as a one-key map — the same shape as Report.
type SessionsCommand struct {
	Sessions struct {
		Most uint32 `cbor:"most" json:"most"`
	} `cbor:"sessions" json:"sessions"`
}

// AskForSessions builds the command asking for the last `most` lines.
func AskForSessions(most uint32) SessionsCommand {
	var c SessionsCommand
	c.Sessions.Most = most
	return c
}
