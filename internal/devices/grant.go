package devices

import (
	"context"
	"errors"
	"fmt"
	"os/exec"
	"strings"
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
	// Keep turns a one-session pairing into a lasting one.
	//
	// **A grant of a different kind, and it takes no permissions.** What it
	// changes is not what a device may do but how long it may keep doing it:
	// a pairing made for one session is spent when that session ends, and
	// this is the only way to say "actually, stay". The Devices screen has
	// shown `once` and `spent` since it was written and could do nothing
	// about either.
	Keep
)

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

	// The two that name a device and nothing else.
	if what == Forget || what == Keep {
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
