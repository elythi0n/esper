/*
Package settings reads and changes what this machine is configured to do.

# Why this is not a second copy of the registry

Every setting's kind, its bounds, the words a choice may be and the sentence
explaining it live in the engine, in one table. This package asks for that
table rather than declaring one: `esper-engine config --json` prints it with
the current values and where each came from, and everything here passes it
through.

A second declaration is a second thing to keep in step, and the way that fails
is quiet. A bound tightened in the engine and not here is a screen offering a
value the file will not take; a setting added in the engine and not here is one
nobody can reach.

# Why the engine writes it

The same reason it writes the device store: one writer. The engine validates
against the kind, writes the file atomically and owner-only, and says what the
value was. A second writer would race it, and the failure that matters is a
lost update.
*/
package settings

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os/exec"
	"strings"
)

// MostKeyChars bounds what may be put on the engine's command line as a key.
//
// The longest setting name is `connectivity.overlay_interfaces`, which is 31,
// so this is that with room. It is here because the key comes from a caller.
const MostKeyChars = 64

// MostValueChars bounds a value.
//
// The longest honest one is an interface list or a broker key: sixty-four hex
// characters, or a handful of names. A value longer than this is not a
// setting, and the engine's own parser is what decides whether what is inside
// the bound means anything.
const MostValueChars = 512

// ErrHostOnly is returned for a setting that may only be changed at the
// machine.
//
// **Not a limitation to work around.** Four of these settings decide whether
// the person in front of the machine can tell what is happening to it, or
// widen what a paired device may reach without anybody granting it anything.
// A request arriving over a socket has nobody standing at the machine, so the
// honest answer is a refusal naming the command to run there.
var ErrHostOnly = errors.New("that setting is changed at the machine")

// ErrNoSuchSetting is returned for a key that is not shaped like one.
var ErrNoSuchSetting = errors.New("that is not a setting's name")

/*
hostOnly is the settings a request may not change, and why each one is here.

The rule is not "dangerous" — every setting here can be got wrong. It is the
two cases where changing one from somewhere else takes away a decision that
belongs to whoever is at the machine:

  - it removes their ability to *know* what is happening, or
  - it widens what a remote device may reach, without a grant.

Everything else is offered. The control plane answers a loopback caller
holding a token no page can read, which is to say somebody already at this
machine — and a product that made them open a terminal for `host.fps` would be
a product with a settings screen that does not work.
*/
//nolint:gochecknoglobals // a considered list, held to the engine's by a test
var hostOnly = map[string]string{
	"host.serve_without_indicator": "turning it on would let someone watch this screen with no visible warning",
	"audit.retain":                 "shortening it deletes the record of past sessions",
	"audit.max_size":               "lowering it deletes the record of past sessions",
	"files.root":                   "it decides which folder paired devices can read",
	"forward.allow":                "it decides which services paired devices can reach through this machine",
	"broker.key":                   "it is a secret, so it is typed at the machine it protects",
}

// HostOnly says whether this setting may only be changed at the machine, and
// why.
func HostOnly(key string) (string, bool) {
	why, found := hostOnly[key]
	return why, found
}

// Writer reads and changes the settings by asking the engine to.
type Writer struct {
	// Engine is the path to the `esper-engine` binary.
	Engine string
	// Home is passed as `ESPER_HOME`, so the engine reads and writes the file
	// this machine uses rather than one belonging to whoever's environment the
	// API inherited.
	Home string
}

// Answer is the registry as the engine prints it, passed through.
//
// The fields are named for what the engine writes rather than restated: this
// is a pass-through, and a struct that renamed anything would be a translation
// to keep in step.
type Answer struct {
	// Which file the values came from, or null where there is no home.
	File *string `json:"file"`
	// Whether that file exists. A machine with no file is configured entirely
	// by defaults, which is the ordinary case and not a fault.
	Present  bool      `json:"present"`
	Settings []Setting `json:"settings"`
}

// Setting is one row of the registry.
type Setting struct {
	Key   string `json:"key"`
	About string `json:"about"`
	// How to write one, as a person reads it.
	Shape string `json:"shape"`
	// "text", "flag", "whole", "whole_or_none", "cores", "address", "key",
	// "choice" or "time".
	Kind string `json:"kind"`
	// Where the value in force came from: a default, the file, the
	// environment, or a flag.
	Source string `json:"source"`
	// The value, in its own type: a number, a boolean, a string, or null for
	// a setting deliberately set to nothing.
	Value any `json:"value"`
	// The same, as the file would write it and as a change takes it back.
	Raw string `json:"raw"`
	// What it is when nobody says.
	Default any `json:"default"`
	// The bounds, for a number.
	Least *uint64 `json:"least,omitempty"`
	Most  *uint64 `json:"most,omitempty"`
	// The words, for a choice.
	Choices []string `json:"choices,omitempty"`
	// Whether a request may change it, and why not — added here rather than
	// by the engine, because it is a fact about *this* boundary and not about
	// the setting.
	AtTheMachine bool   `json:"at_the_machine"`
	WhyAtMachine string `json:"why_at_the_machine,omitempty"`
}

// Read asks the engine for every setting and what it is now.
//
// # Errors
//
// If there is no engine, if it will not run, or if what it printed is not the
// registry.
func (w Writer) Read(ctx context.Context) (*Answer, error) {
	out, err := w.run(ctx, "config", "--json")
	if err != nil {
		return nil, err
	}
	var answer Answer
	if err := json.Unmarshal([]byte(out), &answer); err != nil {
		return nil, fmt.Errorf("the engine's settings could not be read: %w", err)
	}
	// A floor: an answer with nothing in it agrees with everything, and the
	// screen above this would draw an empty page rather than say so.
	if len(answer.Settings) == 0 {
		return nil, errors.New("the engine listed no settings at all, which is not this build")
	}
	for i := range answer.Settings {
		why, only := HostOnly(answer.Settings[i].Key)
		answer.Settings[i].AtTheMachine = only
		answer.Settings[i].WhyAtMachine = why
	}
	return &answer, nil
}

// Set changes one setting, or refuses.
//
// # Errors
//
// If the key is not shaped like a setting's name, if it is one that may only
// be changed at the machine, or if the engine refuses the value — in which
// case what the engine said is the error.
func (w Writer) Set(ctx context.Context, key, value string) error {
	if err := allowed(key); err != nil {
		return err
	}
	if len(value) > MostValueChars {
		return fmt.Errorf("a value of %d characters is not a setting's", len(value))
	}
	// **A value may say anything the setting's kind allows, and reaches the
	// engine as one argument.** It is never a command however it is spelled:
	// there is no shell here, and the engine is what decides whether what is
	// inside the bound means anything.
	_, err := w.run(ctx, "config", "set", key, value)
	return err
}

// Unset takes a setting out of the file, so the default decides again.
//
// # Errors
//
// As [Writer.Set].
func (w Writer) Unset(ctx context.Context, key string) error {
	if err := allowed(key); err != nil {
		return err
	}
	_, err := w.run(ctx, "config", "unset", key)
	return err
}

// allowed refuses a key this boundary will not carry.
//
// The shape first, because a key that is not one cannot be in the host-only
// list and would otherwise be handed to the engine to be refused there — a
// round trip to learn something knowable here.
func allowed(key string) error {
	if key == "" || len(key) > MostKeyChars {
		return fmt.Errorf("%w: %q", ErrNoSuchSetting, key)
	}
	// **An allowlist of the shape a setting's name has, not an escape.** The
	// names are lowercase words separated by dots and underscores; anything
	// else is refused here rather than sanitised, and a name beginning with a
	// `-` is not a key at all but a *flag* the engine would read.
	for _, c := range key {
		ok := (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '.' || c == '_'
		if !ok {
			return fmt.Errorf("%w: %q is not part of one", ErrNoSuchSetting, c)
		}
	}
	if why, only := HostOnly(key); only {
		return fmt.Errorf(
			"%w: run `esper config set %s <value>` at the machine — %s",
			ErrHostOnly, key, why,
		)
	}
	return nil
}

// run asks the engine, and gives back what it said.
func (w Writer) run(ctx context.Context, args ...string) (string, error) {
	if w.Engine == "" {
		return "", errors.New("no engine to ask: the settings are only written by one")
	}
	// An empty home makes every path the engine resolves relative, so the file
	// it wrote would be in whatever directory this process started in — and
	// the answer read back afterwards would come from the real one.
	if strings.TrimSpace(w.Home) == "" {
		return "", errors.New("no home to write in: the settings have to be somewhere")
	}
	cmd := exec.CommandContext(ctx, w.Engine, args...) // #nosec G204 -- the key is matched against an allowlist above, the value is one argument and never a command, and the engine is resolved beside this binary
	cmd.Env = append(cmd.Environ(), "ESPER_HOME="+w.Home)
	out, err := cmd.Output()
	if err == nil {
		return string(out), nil
	}
	// What the engine said, not "exit status 1": it is the half that says what
	// to do next.
	said := ""
	var exited *exec.ExitError
	if errors.As(err, &exited) {
		said = strings.TrimSpace(string(exited.Stderr))
	}
	if said == "" {
		return "", fmt.Errorf("%s %s: %w", w.Engine, strings.Join(args, " "), err)
	}
	return "", errors.New(said)
}
