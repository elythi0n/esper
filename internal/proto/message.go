package proto

import "unicode"

// Role is which side of a session a peer is.
type Role string

// The two roles.
const (
	RoleHost       Role = "host"
	RoleController Role = "controller"
)

// DeviceKeyLen is the length of an Ed25519 public key.
const DeviceKeyLen = 32

// MaxNameLen is the longest accepted peer-supplied display name, in bytes.
// Peer-supplied and therefore untrusted: bounded so a hostile name cannot
// become a memory or layout problem, and never interpreted as markup, a path,
// or a shell word.
const MaxNameLen = 64

// Hello is the first control message in both directions.
//
// The cbor tags are the wire contract and must match the Rust struct's field
// names exactly. Feature evolution happens through Caps, which is intersected —
// never through a version comparison in feature code.
type Hello struct {
	Proto  uint16   `cbor:"proto"`
	Engine string   `cbor:"engine"`
	Device []byte   `cbor:"device"`
	Name   string   `cbor:"name"`
	Role   Role     `cbor:"role"`
	Caps   []string `cbor:"caps"`
}

// NewHello builds a Hello for this build, truncating an over-long name to
// MaxNameLen on a UTF-8 boundary.
func NewHello(role Role, device []byte, name, engine string) Hello {
	key := make([]byte, DeviceKeyLen)
	copy(key, device)
	return Hello{
		Proto:  ProtocolVersion,
		Engine: engine,
		Device: key,
		Name:   truncateOnRuneBoundary(name, MaxNameLen),
		Role:   role,
		Caps:   []string{},
	}
}

// VersionMatches reports whether the peer's protocol version matches ours.
func (h Hello) VersionMatches() bool { return h.Proto == ProtocolVersion }

// CommonCaps returns the capabilities both peers advertise, sorted and
// deduplicated. Set intersection is the only sanctioned way to decide what a
// session supports; comparing versions in feature code is how a protocol rots.
func (h Hello) CommonCaps(other Hello) []string {
	theirs := make(map[string]struct{}, len(other.Caps))
	for _, c := range other.Caps {
		theirs[c] = struct{}{}
	}
	seen := make(map[string]struct{}, len(h.Caps))
	common := make([]string, 0, len(h.Caps))
	for _, c := range h.Caps {
		if _, ok := theirs[c]; !ok {
			continue
		}
		if _, dup := seen[c]; dup {
			continue
		}
		seen[c] = struct{}{}
		common = append(common, c)
	}
	sortStrings(common)
	return common
}

// NameIsAcceptable rejects a received name that is over-long or contains
// control characters. Applied to a decoded Hello: the sender is not trusted to
// have used NewHello.
func (h Hello) NameIsAcceptable() bool {
	if len(h.Name) > MaxNameLen {
		return false
	}
	for _, r := range h.Name {
		if unicode.IsControl(r) {
			return false
		}
	}
	return true
}

// DeviceKeyIsWellFormed reports whether the device key is exactly the expected
// length. A decoded Hello may carry any length, so this is checked before the
// key is used for anything.
func (h Hello) DeviceKeyIsWellFormed() bool { return len(h.Device) == DeviceKeyLen }

// truncateOnRuneBoundary shortens s to at most max bytes without splitting a
// UTF-8 character.
func truncateOnRuneBoundary(s string, max int) string {
	if len(s) <= max {
		return s
	}
	end := max
	for end > 0 && !isRuneStart(s[end]) {
		end--
	}
	return s[:end]
}

// isRuneStart reports whether b is the first byte of a UTF-8 sequence, i.e. not
// a 10xxxxxx continuation byte.
func isRuneStart(b byte) bool { return b&0xC0 != 0x80 }

// sortStrings is an insertion sort. The slices here are capability lists of a
// handful of entries; pulling in sort for that is not worth the import.
func sortStrings(s []string) {
	for i := 1; i < len(s); i++ {
		for j := i; j > 0 && s[j] < s[j-1]; j-- {
			s[j], s[j-1] = s[j-1], s[j]
		}
	}
}
