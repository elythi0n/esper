package devices

import (
	"encoding/json"
	"fmt"
)

// Permission is one thing a device may be allowed to do.
//
// # Why the vocabulary exists twice
//
// `esper-proto` defines it in Rust because it is both halves of one thing: the
// word a controller uses to ask, and the word a host uses to grant. This is
// the same vocabulary in Go, needed because the control plane reads the device
// store and **a store Rust refuses must not be a store Go accepts**. Rust
// deserialises these into an enum, so an unknown word there fails the whole
// file; if Go shrugged at the same word it would list devices the engine will
// never load.
//
// The two are held to each other by `scripts/check-vocabularies.py`, which
// fails if either side gains a word the other does not have.
type Permission string

// What is deliberately **not** here: `is_sensitive`, the rule about which
// permissions need a person to agree rather than a checkbox, and a parser for
// a name somebody typed. Both belong to granting, and nothing here grants.
//
// The rule for what this package holds: **a trivial accessor over a field it
// already has, yes; a decision that nothing makes yet, no.** Rust's own
// `is_sensitive` shipped with every reference in a test and a grant path that
// never consulted it, so `grant <fp> unattended` wrote permanent unattended
// access and printed a success line. A second copy of that rule, in a language
// with no grant path to consult it, is that defect with a place to happen.

// The permissions, in the order Rust lists them.
const (
	View         Permission = "view"
	Control      Permission = "control"
	ClipboardOut Permission = "clipboard_out"
	ClipboardIn  Permission = "clipboard_in"
	FilesOut     Permission = "files_out"
	FilesIn      Permission = "files_in"
	Audio        Permission = "audio"
	Terminal     Permission = "terminal"
	Forward      Permission = "forward"
	Unattended   Permission = "unattended"
	Record       Permission = "record"
	Lock         Permission = "lock"
	Logoff       Permission = "logoff"
	Reboot       Permission = "reboot"
)

// AllPermissions is every permission there is.
//
//nolint:gochecknoglobals // a frozen vocabulary, held to the Rust one by a gate
var AllPermissions = []Permission{
	View, Control, ClipboardOut, ClipboardIn, FilesOut, FilesIn,
	Audio, Terminal, Forward, Unattended, Record, Lock, Logoff, Reboot,
}

// Sensitive says whether granting this needs a person at the machine rather
// than a checkbox.
//
// **It was written, removed for having no caller, and is back because it has
// one.** The removal was right at the time: Rust's own `is_sensitive` shipped
// with every reference in a test and a grant path that never consulted it, so
// `grant <fp> unattended` wrote permanent unattended access and printed a
// success line. A second copy of that rule with nothing consulting it would
// have been the same defect in a second language.
//
// Its caller is [command], which refuses to grant any of these through the
// local API — see [ErrHostOnly].
func (p Permission) Sensitive() bool {
	switch p {
	// Removes the question a session is otherwise held up by.
	case Unattended,
		// Writes to this machine's filesystem.
		FilesIn,
		// Keeps a copy of somebody's screen after the session.
		Record,
		// Closes what somebody is working on.
		Logoff,
		// Takes the machine away, and every other session on it.
		Reboot:
		return true
	case View, Control, ClipboardOut, ClipboardIn, FilesOut, Audio, Terminal, Forward, Lock:
		return false
	}
	return false
}

// Known says whether this is a permission this build understands.
func (p Permission) Known() bool {
	for _, known := range AllPermissions {
		if p == known {
			return true
		}
	}
	return false
}

// UnmarshalJSON refuses a word this build does not know.
//
// Refused rather than kept as an opaque string, because a permission nobody
// can interpret is one nobody can show, revoke, or reason about — and because
// Rust refuses it, so accepting it here would put the two out of step about
// which stores are readable at all.
func (p *Permission) UnmarshalJSON(b []byte) error {
	var word string
	if err := json.Unmarshal(b, &word); err != nil {
		return fmt.Errorf("a permission is a word: %w", err)
	}
	got := Permission(word)
	if !got.Known() {
		return fmt.Errorf("%q is not a permission this build knows", word)
	}
	*p = got
	return nil
}

// LockAfter says whether a session with a device leaves the screen locked.
//
// Absent from a device record means the host's own setting decides, which is
// why the store holds a pointer and not a value.
type LockAfter string

// The lock-after choices, in the order Rust lists them.
const (
	// LockWhenUnattended locks after a session nobody was present for.
	LockWhenUnattended LockAfter = "unattended"
	// LockAlways locks after every session.
	LockAlways LockAfter = "always"
	// LockNever leaves the screen as the session left it.
	LockNever LockAfter = "never"
)

// AllLockAfter is every choice there is.
//
//nolint:gochecknoglobals // a frozen vocabulary, held to the Rust one by a gate
var AllLockAfter = []LockAfter{LockWhenUnattended, LockAlways, LockNever}

// Known says whether this is a choice this build understands.
func (l LockAfter) Known() bool {
	for _, known := range AllLockAfter {
		if l == known {
			return true
		}
	}
	return false
}

// UnmarshalJSON refuses a word this build does not know, for the same reason
// [Permission.UnmarshalJSON] does.
func (l *LockAfter) UnmarshalJSON(b []byte) error {
	var word string
	if err := json.Unmarshal(b, &word); err != nil {
		return fmt.Errorf("a lock-after choice is a word: %w", err)
	}
	got := LockAfter(word)
	if !got.Known() {
		return fmt.Errorf("%q is not something this build knows to do after a session", word)
	}
	*l = got
	return nil
}
