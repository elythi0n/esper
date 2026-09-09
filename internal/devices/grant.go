package devices

import (
	"context"
	"errors"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

// Change is what to do to a device's permissions.
type Change int

const (
	// Grant adds permissions.
	Grant Change = iota
	// Revoke takes them away.
	Revoke
	// Forget removes the device entirely.
	Forget
	// Schedule bounds *when* `unattended` applies.
	//
	// **Narrowing, never widening — which is why it is not gated the way a
	// grant is.** The permission itself is granted at the machine and stays
	// that way; this says the hours inside which it counts, and every
	// schedule is a subset of the `always` a bare grant means. There is no
	// version of this that gives a device more than it already had.
	Schedule
	// Keep turns a one-session pairing into a lasting one.
	//
	// **A grant of a different kind, and it takes no permissions.** What it
	// changes is not what a device may do but how long it may keep doing it:
	// a pairing made for one session is spent when that session ends, and
	// this is the only way to say "actually, stay". The Devices screen has
	// shown `once` and `spent` since it was written and could do nothing
	// about either.
	Keep
	// LockAfterChange says whether a session with this device leaves the
	// screen locked, overriding the host-wide setting for one device.
	//
	// **The one change here that can leave a machine less protected**, since
	// `never` is a real answer and the host's own setting may be `always`. It
	// is offered anyway, for the reason the engine offers it: a laptop
	// connecting to its owner's own desktop should not lock it under them.
	// What keeps it safe is what keeps `revoke` and `forget` safe — the
	// control plane answers a loopback caller holding a token no page can
	// read, which is to say somebody already at this machine.
	LockAfterChange
	// Wake sends a magic packet to a device asleep on this network.
	//
	// **Not a change to anything, and here anyway.** What this package holds
	// is every decision about running one of the engine's per-device verbs —
	// the fingerprint rule, the argv, the timeout, the four ways it can fail —
	// and a wake needs all of them and nothing else. A second path beside this
	// one would be a second place for the fingerprint rule to be slightly
	// different.
	//
	// **The broadcast address only.** The engine takes `--to <address>` for a
	// segment this machine is not on; the control plane does not pass it,
	// because that would let a caller name where a packet goes. A person at a
	// terminal can say it, and nothing arriving over a socket can.
	Wake
	// LockScreen locks a paired host's screen, and nothing else.
	//
	// **An action on the far machine, not a change to anything here.** The
	// three below are the only verbs in this package that reach outward, and
	// what they may do is decided at the *other* end: the permission is one
	// that host granted this one, so this cannot know in advance whether it
	// will be allowed. It asks, and what comes back is the host's own answer.
	LockScreen
	// LogOff ends the login session on a paired host.
	LogOff
	// Restart restarts a paired host.
	//
	// **The most consequential one there is**, because the effect outlives the
	// session that asked for it. The far host is what refuses this without the
	// permission, and granting that permission needed confirming at the
	// machine — so there is nothing further for this end to gate.
	Restart
	// Group puts a device in a named group.
	//
	// **A group is an address, not a grant.** `esper grant --group ops
	// control` resolves the members and writes the permission onto each of
	// them, so membership by itself gives a device nothing — which is why
	// this is not gated the way a grant is. What it changes is who a later
	// command is about.
	Group
	// Ungroup takes a device out of one. The last member leaving takes the
	// group's name with it, which is what makes a group its members.
	Ungroup
)

// MostGroupChars bounds a group's name.
//
// Long enough for a team, a room or a site; short enough that a name is a
// name. It is here because the name comes from a caller and reaches a command
// line, where the engine reads it as the thing the rest of the arguments are
// about.
const MostGroupChars = 32

// Reaching says whether this change is an action on another machine.
//
// Those take a network round trip rather than a file write, so they are given
// longer and answered differently: a machine that is restarting does not reply
// at all.
func (c Change) Reaching() bool {
	return c == Wake || c == LockScreen || c == LogOff || c == Restart
}

// LockDefault takes a per-device answer off, so the host's setting decides.
//
// The engine reads several spellings of this; the control plane takes one, so
// there is a single thing for a caller to send and a single thing to test.
const LockDefault = "default"

// The engine's own subcommand for each.
func (c Change) verb() string {
	switch c {
	case Grant:
		return "grant"
	case Revoke:
		return "revoke"
	case Forget:
		return "forget"
	case Keep:
		return "keep"
	case Schedule:
		return "schedule"
	case LockAfterChange:
		return "lock-after"
	case Wake:
		return "wake"
	case LockScreen:
		return "lock"
	case LogOff:
		return "logoff"
	case Restart:
		return "reboot"
	case Group, Ungroup:
		return "group"
	}
	return ""
}

// ErrHostOnly is returned for a permission that may only be granted by
// somebody at the machine.
//
// **Not a limitation to work around.** `esper grant <fp> unattended` asks for
// confirmation at the host, and that prompt is the entire reason `unattended`
// cannot be switched on silently — it is the permission that *removes* the
// question a session is otherwise held up by. A request arriving over the
// network has nobody to ask, so the honest answer is a refusal naming the
// command to run at the machine. Passing `--yes` on its behalf would answer a
// question about physical presence from something that has none.
var ErrHostOnly = errors.New("that permission is granted at the machine")

// ErrNoSuchDevice is returned for a fingerprint that is not 16 hex characters.
var ErrNoSuchDevice = errors.New("that is not a device fingerprint")

// FingerprintHex is how many characters a full fingerprint has.
//
// Eight bytes, so sixteen characters. The engine's own commands accept a
// *prefix* and refuse an ambiguous one, which is right for a person typing —
// but a caller that got the fingerprint from `GET /devices` has all of it, and
// a prefix that is unambiguous today can match a second device tomorrow. So
// this requires the whole thing.
const FingerprintHex = FingerprintWords * 2

// MostScheduleChars bounds what may be put on the engine's command line.
//
// The longest honest one is seven day names and a window — `mon,tue,wed,thu,
// fri,sat,sun 08:00-18:00`, which is 39 — so this is that with room and
// nothing like enough for a second schedule glued onto the first. It is here
// because the string comes from a caller, and the engine's parser is the thing
// that decides what a schedule *means*: this only decides what is worth
// handing it.
const MostScheduleChars = 48

// command builds the engine's argv for a change, or refuses it.
//
// **Every decision this package makes about a change is here**, and the call
// that runs it is five lines — because this can be tested on any machine and
// spawning a process to find out what argv was built cannot.
//
// # Errors
//
// If the fingerprint is not a full one, if a permission is unknown, if none
// were named where some are needed, or if a grant names one that may only
// happen at the machine.
func command(what Change, fingerprint string, perms []Permission) ([]string, error) {
	if len(fingerprint) != FingerprintHex || !isLowerHex(fingerprint) {
		return nil, fmt.Errorf(
			"%w: %q is not %d lowercase hex characters — use the `hex` from the "+
				"device list, all of it",
			ErrNoSuchDevice, fingerprint, FingerprintHex,
		)
	}

	// A schedule names a device and a when, and the when is one argument.
	if what == Schedule {
		if len(perms) != 1 {
			return nil, errors.New("schedule takes exactly one when, like `weekdays 08:00-18:00`")
		}
		when := strings.TrimSpace(string(perms[0]))
		if when == "" {
			return nil, errors.New("schedule needs a when, or `clear` to take one off")
		}
		if len(when) > MostScheduleChars {
			return nil, fmt.Errorf("a schedule of %d characters is not one", len(when))
		}
		// **It has to start with a letter**, because every schedule does —
		// `weekdays`, `daily`, `always`, `clear`, `mon` — and because a `when`
		// beginning with `-` is not a schedule the engine reads, it is a
		// *flag* it reads. The character allowlist below admits `--yes`
		// happily; this is what stops it being an argument.
		if c := when[0]; c < 'a' || c > 'z' {
			return nil, fmt.Errorf(
				"a schedule starts with a day or a word like `weekdays`; %q does not", when)
		}
		// **An allowlist of the shape a schedule has, not an escape.** It
		// reaches a command line, and what a schedule is made of is day names,
		// digits, colons, commas, a hyphen and one space. Anything else is
		// refused here rather than sanitised — the engine's parser is what
		// decides whether those characters spell a schedule it knows.
		for _, c := range when {
			ok := (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') ||
				c == ':' || c == ',' || c == '-' || c == ' '
			if !ok {
				return nil, fmt.Errorf(
					"a schedule is day names, hours and `-`; %q is not part of one", c)
			}
		}
		return []string{what.verb(), fingerprint, when}, nil
	}

	// A lock-after names a device and one word out of a closed set.
	//
	// **Checked here rather than passed through**, unlike a schedule: a
	// schedule is a grammar the engine's parser owns, and this is four words
	// that already exist on this side. Checking them means a caller is told
	// which words there are instead of being handed the engine's usage, and
	// means nothing but one of those words can reach a command line.
	if what == LockAfterChange {
		if len(perms) != 1 {
			return nil, fmt.Errorf(
				"lock-after takes exactly one of %s or %q", joinLockAfter(), LockDefault)
		}
		word := strings.TrimSpace(string(perms[0]))
		if word != LockDefault && !LockAfter(word).Known() {
			return nil, fmt.Errorf(
				"%q is not something this build knows to do after a session — it is one of %s or %q",
				word, joinLockAfter(), LockDefault,
			)
		}
		return []string{what.verb(), fingerprint, word}, nil
	}

	/*
		A group names the group first and the device second, and that order is
		the whole reason this is checked here rather than passed through.

		`group <name> <fingerprint>...` reads its first argument as the group.
		A name that began with `-` would be a *flag* in that position, and one
		carrying a space would be two arguments — the second of which the
		engine would read as a fingerprint. So the name is held to the shape a
		name has: letters, digits, and the two separators people put in one.
	*/
	if what == Group || what == Ungroup {
		if len(perms) != 1 {
			return nil, errors.New("a group change names exactly one group")
		}
		name := strings.TrimSpace(string(perms[0]))
		if name == "" {
			return nil, errors.New("a group needs a name")
		}
		// Counted in characters rather than bytes: a name in a script that
		// takes three bytes a letter is not three times as long.
		if utf8.RuneCountInString(name) > MostGroupChars {
			return nil, fmt.Errorf(
				"a group name of %d characters is not one", utf8.RuneCountInString(name))
		}
		/*
			**Letters in any script, not only the twenty-six.** A product whose
			device list holds `デスクトップ · مكتب` has people who will name a
			group in their own language, and an ASCII allowlist would make
			those groups ones the window can show and never change. The store
			already holds one.

			`unicode.IsLetter` and `IsDigit` are the L* and Nd categories: no
			spaces, no control characters, no punctuation, and none of the
			format characters that make a name display as something other than
			what it is. The two separators people put in a name are added by
			hand.
		*/
		first, _ := utf8.DecodeRuneInString(name)
		if !unicode.IsLetter(first) && !unicode.IsDigit(first) {
			return nil, fmt.Errorf("a group's name starts with a letter or a digit; %q does not", name)
		}
		for _, c := range name {
			if !unicode.IsLetter(c) && !unicode.IsDigit(c) && c != '-' && c != '_' {
				return nil, fmt.Errorf(
					"a group's name is letters, digits, `-` and `_`; %q is not part of one", c)
			}
		}
		args := []string{what.verb(), name, fingerprint}
		if what == Ungroup {
			args = append(args, "--remove")
		}
		return args, nil
	}

	// The ones that name a device and nothing else.
	if what == Forget || what == Keep || what == Wake ||
		what == LockScreen || what == LogOff || what == Restart {
		if len(perms) != 0 {
			return nil, fmt.Errorf("%s does not take permissions", what.verb())
		}
		return []string{what.verb(), fingerprint}, nil
	}

	if len(perms) == 0 {
		return nil, fmt.Errorf("%s needs at least one permission", what.verb())
	}
	seen := make(map[Permission]bool, len(perms))
	args := make([]string, 0, len(perms)+2)
	args = append(args, what.verb(), fingerprint)
	for _, p := range perms {
		if !p.Known() {
			return nil, fmt.Errorf("%q is not a permission this build knows", p)
		}
		if seen[p] {
			// Refused rather than deduplicated: a caller that named a
			// permission twice has a bug, and granting it once quietly is how
			// the bug survives.
			return nil, fmt.Errorf("%q is named twice", p)
		}
		seen[p] = true
		// **Only a grant is gated.** Taking access away needs no ceremony:
		// there is no version of "are you sure you want less" worth asking,
		// and a revocation that could be refused would be a revocation
		// somebody could be talked out of.
		if what == Grant && p.Sensitive() {
			return nil, fmt.Errorf(
				"%w: run `esper grant %s %s` at the machine — it asks whoever is "+
					"there, and that question is why this one cannot be switched on "+
					"from somewhere else",
				ErrHostOnly, fingerprint, p,
			)
		}
		args = append(args, string(p))
	}
	return args, nil
}

// The choices, for the sentence a refusal ends with.
func joinLockAfter() string {
	words := make([]string, 0, len(AllLockAfter))
	for _, a := range AllLockAfter {
		words = append(words, strconv.Quote(string(a)))
	}
	return strings.Join(words, ", ")
}

func isLowerHex(s string) bool {
	for _, c := range s {
		if (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return false
		}
	}
	return true
}

// Writer changes the device store by asking the engine to.
//
// # Why not just write the file
//
// Because there would then be two writers of the file that says who this
// machine trusts, and the failure that matters is a lost update: two changes
// racing, one of them putting back a permission somebody revoked. The engine
// is the writer — it is where pairing and the key exchange are, its save is
// atomic and owner-only, and it is what records the audit entry and asks for
// confirmation where confirmation is required.
//
// So this builds an argv and runs it. Nothing here opens the store for
// writing, which is why [`Load`] and this can coexist without a lock.
type Writer struct {
	// Engine is the path to the `esper-engine` binary.
	Engine string
	// Home is passed as `ESPER_HOME`, so the engine writes the store this
	// package reads rather than the one belonging to whoever's environment
	// the API inherited.
	Home string
}

// Change asks the engine to make one.
//
// # Errors
//
// If the request is one this refuses — see [command] — or if the engine
// refuses it, in which case what the engine said is the error.
func (w Writer) Change(ctx context.Context, what Change, fingerprint string, perms []Permission) error {
	args, err := command(what, fingerprint, perms)
	if err != nil {
		return err
	}
	if w.Engine == "" {
		return errors.New("no engine to ask: the device store is only written by one")
	}
	// **An empty home is refused rather than passed on.** `ESPER_HOME=` makes
	// every path the engine resolves relative, so the store it wrote would be
	// in whatever directory this process started in — and the answer read back
	// afterwards would come from the real store, which the write never
	// touched. The engine refuses it too; this refuses it before spawning a
	// process to be told so.
	if strings.TrimSpace(w.Home) == "" {
		return errors.New("no home to write in: the device store has to be somewhere")
	}
	// An argv rather than a shell, so a fingerprint is an argument and can
	// never be a command however it is spelled — and it is checked above
	// besides.
	cmd := exec.CommandContext(ctx, w.Engine, args...)
	cmd.Env = append(cmd.Environ(), "ESPER_HOME="+w.Home)
	out, err := cmd.CombinedOutput()
	if err == nil {
		return nil
	}
	// What the engine said, not "exit status 1". It is the half that tells
	// somebody what to do next.
	said := strings.TrimSpace(string(out))
	if said == "" {
		return fmt.Errorf("%s %s: %w", w.Engine, args[0], err)
	}
	return errors.New(said)
}
