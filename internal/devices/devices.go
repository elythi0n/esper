// Package devices reads the paired-device store: who this machine trusts and
// what each of them may do.
//
// # Why the control plane reads the file rather than asking the engine
//
// Every other question this control plane answers — status, sessions, events —
// it answers by asking a running engine. The device list cannot work that way.
// A machine with nothing connected has no engine running, and that is exactly
// when somebody opens the device list: to pick something to connect to. An
// answer available only while a session is already open would be an answer
// nobody could use.
//
// Reading the file is not a shortcut around the engine, because the file is
// not the engine's private state. The engine re-reads it while it runs, which
// is what makes a revocation reach a session already in progress no matter
// which process made it. The file is the agreed source of truth, and this is
// the same contract from the other side.
//
// # Why this package does not write
//
// Deliberately, for now. Pairing *creates* records and pairing is where the
// key exchange is, so it is Rust's. A second writer of a trust store risks the
// one failure that actually matters here: a lost update putting back a
// permission somebody revoked. Until there is a single-writer path worth
// trusting, writes stay in the one place that makes them.
//
// # The one check that is not here
//
// Rust drops an entry whose key is not a usable Ed25519 point, and says which
// it dropped. Go cannot make that check without a second implementation of
// curve arithmetic, and a security check implemented twice is two checks that
// can disagree.
//
// So this package does not make it, and does not pretend to: an entry with an
// unusable key is listed here and refused by the engine. That is the safe
// direction of the two — a device shown that cannot connect, rather than a
// device hidden that can — and it is only reachable by editing the file by
// hand, since [Permission] and the store's own writer both refuse such a key
// on the way in.
//
// Everything decidable from the text *is* checked, and matched to Rust
// exactly: an unknown format version, a malformed key, an unknown permission
// and an unknown lock-after choice all refuse the whole file on both sides.
package devices

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
)

// KeyLen is the length of a device's public key in bytes.
const KeyLen = 32

// Version is the store format this build understands.
const Version uint32 = 1

// Key is a device's Ed25519 public key. This *is* the device.
type Key [KeyLen]byte

// String renders the key as the lowercase hex the store is written in.
func (k Key) String() string { return hex.EncodeToString(k[:]) }

// MarshalText writes the key as lowercase hex.
//
// Text rather than JSON so a key can also be a map key, which is what the
// store files its entries under.
func (k Key) MarshalText() ([]byte, error) { return []byte(k.String()), nil }

// UnmarshalText reads a 64-character hex key.
//
// Uppercase hex is accepted and normalised, because Rust's reader accepts it
// too and the point of holding the two sides together is that neither takes a
// file the other refuses. Any other malformed key fails the whole file rather
// than dropping one entry — a trust store with a character wrong in it is a
// file somebody should look at, not one to quietly serve a subset of.
func (k *Key) UnmarshalText(text []byte) error {
	if len(text) != KeyLen*2 {
		return fmt.Errorf("a key is %d hex characters, and this one is %d", KeyLen*2, len(text))
	}
	raw, err := hex.DecodeString(string(text))
	if err != nil {
		return fmt.Errorf("a key is hex: %w", err)
	}
	*k = Key(raw)
	return nil
}

// Pairing says how long a pairing lasts.
//
// The zero value is a pairing kept until somebody removes it, which is what
// pairing in person produces and what a store written before one-session
// pairings existed means.
type Pairing struct {
	// Once is set for a pairing spent by a single session.
	Once *Once `json:"once,omitempty"`
}

// Once is a pairing good for one session, with a deadline of its own.
type Once struct {
	// Used is set when the session opens, not when it ends.
	Used bool `json:"used"`
	// ExpiresAt is Unix seconds after which it lapses even unused.
	ExpiresAt uint64 `json:"expires_at"`
}

// Kept says whether this pairing lasts until somebody removes it.
func (p Pairing) Kept() bool { return p.Once == nil }

// Admits says whether this pairing still lets a session open.
func (p Pairing) Admits(now uint64) bool {
	if p.Once == nil {
		return true
	}
	return !p.Once.Used && now < p.Once.ExpiresAt
}

// MarshalJSON writes whichever of the two forms the store uses.
func (p Pairing) MarshalJSON() ([]byte, error) {
	if p.Once == nil {
		return json.Marshal("kept")
	}
	return json.Marshal(map[string]*Once{"once": p.Once})
}

// UnmarshalJSON reads either form the store writes.
//
// Rust writes a unit variant as the bare string `"kept"` and a struct variant
// as `{"once": {...}}`. Reading both by hand is shorter than a Go type that
// could hold either.
func (p *Pairing) UnmarshalJSON(b []byte) error {
	var word string
	if err := json.Unmarshal(b, &word); err == nil {
		if word != "kept" {
			return fmt.Errorf("%q is not a kind of pairing", word)
		}
		p.Once = nil
		return nil
	}
	var wrapped struct {
		Once *Once `json:"once"`
	}
	if err := unmarshalOne(b, &wrapped); err != nil {
		return fmt.Errorf("reading a pairing: %w", err)
	}
	if wrapped.Once == nil {
		return errors.New("a pairing that is neither kept nor once")
	}
	p.Once = wrapped.Once
	return nil
}

// Device is one paired device, as the store holds it.
//
// The fields are the file's, named as the file names them, so there is nothing
// here that can disagree with it. What a person is shown — the fingerprint,
// whether a pairing has lapsed — is derived rather than stored.
type Device struct {
	// PublicKey is the identity. Everything else is a label on it.
	PublicKey Key `json:"public_key"`
	// Name is peer-supplied at pairing, editable locally, and untrusted.
	Name string `json:"name"`
	// Permissions is what this device may do.
	Permissions []Permission `json:"permissions"`
	// PairedAt is Unix seconds when pairing completed.
	PairedAt uint64 `json:"paired_at"`
	// LastSeen is Unix seconds of the last completed session, if any.
	LastSeen *uint64 `json:"last_seen"`
	// LastAddress is where this device was last reached. A hint, never an
	// authority: the key check is what decides, so a stale entry costs a
	// failed dial and nothing else.
	LastAddress string `json:"last_address,omitempty"`
	// OverlayAddress is where it was last reached over a network the user
	// already runs — a tunnel, a tailnet. Kept apart from LastAddress
	// because a machine reachable both ways is reachable both ways.
	OverlayAddress string `json:"overlay_address,omitempty"`
	// Pairing is how long the pairing lasts.
	Pairing Pairing `json:"pairing"`
	// UnattendedWhen bounds the unattended permission to certain hours. Held
	// as the text the file holds: this package shows a schedule and does not
	// evaluate one, and re-implementing the parser would be a second thing to
	// keep in step for no gain.
	UnattendedWhen *string `json:"unattended_when,omitempty"`
	// LockAfter overrides the host's own after-session locking for this one
	// device. Absent means the host setting decides.
	LockAfter *LockAfter `json:"lock_after,omitempty"`
	// Groups this device is in, for applying a policy to several at once.
	Groups []string `json:"groups,omitempty"`
	// LastMAC is the hardware address it was last reached at, for waking a
	// machine that is asleep. Absent unless it was reached on a local segment.
	LastMAC string `json:"last_mac,omitempty"`
}

// May says whether this device holds a permission.
func (d Device) May(p Permission) bool {
	for _, held := range d.Permissions {
		if held == p {
			return true
		}
	}
	return false
}

// Fingerprint is a device key rendered as something a person can compare.
type Fingerprint struct {
	// Hex is what `grant`, `revoke` and `forget` take on a command line.
	Hex string `json:"hex"`
	// Words is what somebody reads out to somebody else.
	Words []string `json:"words"`
}

// FingerprintOf derives the fingerprint of a public key.
//
// Hashed with a domain separator rather than truncated: truncating would tie
// the fingerprint to the key's encoding, so a future key type would silently
// change every fingerprint anybody had ever compared. A committed fixture
// holds this to the Rust side byte for byte.
func FingerprintOf(publicKey []byte) Fingerprint {
	sum := sha256.New()
	sum.Write([]byte(FingerprintDomain))
	sum.Write(publicKey)
	digest := sum.Sum(nil)[:FingerprintWords]
	out := Fingerprint{Hex: hex.EncodeToString(digest), Words: make([]string, 0, FingerprintWords)}
	for _, b := range digest {
		out.Words = append(out.Words, words[b])
	}
	return out
}

// Fingerprint derives this device's fingerprint from its key.
func (d Device) Fingerprint() Fingerprint { return FingerprintOf(d.PublicKey[:]) }

// store is the file's own shape.
//
// The map is keyed by the **raw text** the file used, not by a parsed key. Rust
// holds this as a `BTreeMap<String, Device>` and re-keys it in that map's own
// order, so the order has to be the order of the strings as written — and hex
// is not always written the same way. Parsing first would normalise the case
// away and sort a mixed-case store differently from the side that wrote it.
type store struct {
	Version uint32             `json:"version"`
	Devices map[string]*Device `json:"devices"`
}

// FileName is what the store is called inside the Esper home.
const FileName = "devices.json"

// PathIn is where the store lives inside a given Esper home.
func PathIn(dir string) string { return filepath.Join(dir, FileName) }

// Load reads the store at a path.
//
// A file that is **absent is an empty store**, not an error: a machine that
// has never paired is the ordinary state on first run, and it is the answer
// the engine gives too.
//
// A file that cannot be read is an error and stays one. This is the file that
// says who this machine trusts, and answering "nobody is paired" because a
// character is wrong would be the worst available way to fail — a device list
// that looks empty invites somebody to pair again rather than to look at the
// file.
func Load(path string) ([]Device, error) {
	raw, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading %s: %w", path, err)
	}
	return Read(raw, path)
}

// Read parses store bytes. `where` names them in any complaint.
//
// # Errors
//
// If the bytes are not a store, or are a version this build does not
// understand.
func Read(raw []byte, where string) ([]Device, error) {
	var s store
	if err := unmarshalOne(raw, &s); err != nil {
		return nil, fmt.Errorf("%s is not a device store: %w", where, err)
	}
	// **A version this build does not know is refused, not read past.** A
	// store from a later Esper may hold permissions this one cannot name, and
	// showing a device without them is showing the wrong device.
	if s.Version != Version {
		return nil, fmt.Errorf(
			"%s was written by a different version of Esper (format %d, this build "+
				"understands %d); it is not being read",
			where, s.Version, Version,
		)
	}

	// **The record is the identity; the map key is an index into it.** Rust
	// re-keys every entry from its own `public_key` on load, so an entry filed
	// under the wrong key is corrected rather than dropped, and two entries
	// naming one key leave the later of the two. Matched here, including the
	// order, so neither side ever lists a device the other does not.
	filed := make([]string, 0, len(s.Devices))
	for key := range s.Devices {
		filed = append(filed, key)
	}
	sort.Strings(filed)

	byKey := make(map[Key]*Device, len(s.Devices))
	for _, key := range filed {
		byKey[s.Devices[key].PublicKey] = s.Devices[key]
	}

	list := make([]Device, 0, len(byKey))
	for _, device := range byKey {
		list = append(list, *device)
	}
	// Sorted by name, then key, so a listing does not reshuffle between
	// reads: Go randomizes map order on purpose, and a device screen whose
	// rows moved on every refresh would be unusable.
	sort.Slice(list, func(i, j int) bool {
		if list[i].Name != list[j].Name {
			return list[i].Name < list[j].Name
		}
		return list[i].PublicKey.String() < list[j].PublicKey.String()
	})
	return list, nil
}
