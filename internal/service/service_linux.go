//go:build linux

package service

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// Manager is what this platform's service manager is called, for a message.
const Manager = "systemd --user"

// Enable installs the unit and starts the host, now and at login.
//
// # Errors
//
// If `systemctl` is not there, if the unit cannot be written, or if systemd
// refuses. Every command is reported before it runs, so a failure part-way
// leaves something a person can finish by hand.
func Enable(r Report) error {
	if err := haveSystemctl(); err != nil {
		return err
	}
	path, err := installUnit(r)
	if err != nil {
		return err
	}
	r.say("  wrote      %s", path)

	if err := run(r, "systemctl", "--user", "daemon-reload"); err != nil {
		return err
	}
	if err := run(r, "systemctl", "--user", "enable", "--now", unitName); err != nil {
		return err
	}

	// **The step people miss.** Without linger, systemd stops user services
	// when the last session ends — so on a machine nobody logs into, the host
	// stops the moment somebody disconnects, and it looks like a crash.
	//
	// Not enabled here: it needs root, and a tool that quietly asked for a
	// password to change a login-session policy would be doing something
	// bigger than it was asked to. Reported instead, with the command.
	if !lingering() {
		r.say("")
		r.say("  ⚠ linger is off for this account, so systemd will stop the host when")
		r.say("    your last session ends. On a machine you connect *to*, that reads")
		r.say("    as Esper crashing whenever you disconnect. To keep it running:")
		r.say("")
		r.say("        sudo loginctl enable-linger %s", os.Getenv("USER"))
	}
	return nil
}

// Disable stops the host and stops it starting at login.
//
// **The unit is left in place.** Removing it would throw away a file somebody
// may have edited, and `disable` is a reversible thing to ask for — the
// reverse of it should not need the file back.
//
// # Errors
//
// If `systemctl` is not there or refuses.
func Disable(r Report) error {
	if err := haveSystemctl(); err != nil {
		return err
	}
	if err := run(r, "systemctl", "--user", "disable", "--now", unitName); err != nil {
		// **Already off is not a failure.** `disable` asks for a state, not
		// for a transition, and a machine where the unit was never installed
		// is a machine where the host does not start at login — which is what
		// was asked for. Refusing would make the command unusable in exactly
		// the script that wants to be sure.
		if strings.Contains(err.Error(), "does not exist") {
			r.say("  already off: no unit is installed")
			return nil
		}
		return err
	}
	r.say("")
	r.say("  the unit is left at %s, so enabling it again needs no reinstall",
		filepath.Join(unitDir(), unitName))
	return nil
}

// Now says what systemd currently thinks of the host.
//
// Asked rather than remembered: the unit can be enabled, started and stopped
// by anything on the machine — `systemctl` in a terminal, a distribution
// package, a reboot — and a control plane keeping its own copy of the answer
// would be wrong the first time somebody used the tool that owns it.
//
// A failed `systemctl` is a "no", not an error: the question is whether the
// host is on, and a manager that will not answer is a machine where it is
// not.
func Now() State {
	if err := haveSystemctl(); err != nil {
		return State{Why: err.Error()}
	}
	return State{
		Managed: true,
		Enabled: systemctlSays("is-enabled"),
		Active:  systemctlSays("is-active"),
	}
}

// systemctlSays runs one of systemd's yes/no questions.
func systemctlSays(question string) bool {
	// `is-enabled` and `is-active` answer with an exit status, and print a
	// word on stdout that is not always the one the status means — so the
	// status is what is read.
	cmd := exec.Command("systemctl", "--user", question, unitName) // #nosec G204 -- fixed arguments
	return cmd.Run() == nil
}

func haveSystemctl() error {
	if _, err := exec.LookPath("systemctl"); err != nil {
		return fmt.Errorf(
			"%w: this is Linux without systemd, so the host is started by whatever "+
				"init this machine runs — `esper host` is the command to wrap",
			ErrUnsupported,
		)
	}
	return nil
}

func unitDir() string {
	if dir := os.Getenv("XDG_CONFIG_HOME"); dir != "" {
		return filepath.Join(dir, "systemd", "user")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return filepath.Join(".config", "systemd", "user")
	}
	return filepath.Join(home, ".config", "systemd", "user")
}

// ourPath is the absolute path of the running binary, or "" if it cannot be
// had. On Linux this comes from `/proc/self/exe`, so it is already resolved.
func ourPath() string {
	path, err := os.Executable()
	if err != nil {
		return ""
	}
	return path
}

// unitFor is the unit, starting the binary that is installing it.
//
// The embedded copy names `%h/.local/bin/esper`, which is where
// `make install-user` puts it — and that is one of three places it can be. A
// distribution package installs `/usr/bin/esper`; a build from source usually
// goes to `/usr/local/bin`. A unit carrying the wrong one of those installs
// without complaint, enables without complaint, and then fails to start with
// 203/EXEC on a machine where everything looks present.
//
// That is not hypothetical: the .deb installed `/usr/bin/esper` next to a unit
// starting `%h/.local/bin/esper`, and the broker's package did the same with
// `/usr/local/bin`. Both were found by installing the packages in a Debian and
// reading the unit beside the binary.
//
// The binary doing the installing is the one that will be started, and it
// knows where it is, so it says so rather than guessing a prefix.
func unitFor(executable string) string {
	if executable == "" {
		return Unit
	}
	return strings.ReplaceAll(Unit, "%h/.local/bin/esper", executable)
}

// installUnit writes the unit, unless one is already there that this did not
// write.
func installUnit(r Report) (string, error) {
	dir := unitDir()
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", fmt.Errorf("making %s: %w", dir, err)
	}
	path := filepath.Join(dir, unitName)

	wanted := unitFor(ourPath())

	switch existing, err := os.ReadFile(path); {
	case err == nil && string(existing) == wanted:
		r.say("  unchanged  %s", path)
		return path, nil
	case err == nil:
		// **Not overwritten.** A distribution package may have installed one,
		// with different paths and different hardening, and replacing it
		// silently would undo somebody's packaging — or their own edit.
		r.say("  kept       %s (already installed and different from ours)", path)
		return path, nil
	case errors.Is(err, os.ErrNotExist):
	default:
		return "", fmt.Errorf("reading %s: %w", path, err)
	}

	if err := os.WriteFile(path, []byte(wanted), 0o600); err != nil {
		return "", fmt.Errorf("writing %s: %w", path, err)
	}
	return path, nil
}

// lingering says whether this account's services survive its last session.
func lingering() bool {
	out, err := exec.Command("loginctl", "show-user", os.Getenv("USER"), "--property=Linger").Output()
	if err != nil {
		// Cannot tell. Reported as off, because the warning is cheap and the
		// failure it prevents is a host that stops when somebody disconnects.
		return false
	}
	return strings.Contains(string(out), "Linger=yes")
}

// run says what it is about to do, then does it.
func run(r Report, name string, args ...string) error {
	r.say("  running    %s %s", name, strings.Join(args, " "))
	cmd := exec.Command(name, args...)
	out, err := cmd.CombinedOutput()
	if err == nil {
		return nil
	}
	said := strings.TrimSpace(string(out))
	if said == "" {
		return fmt.Errorf("%s %s: %w", name, strings.Join(args, " "), err)
	}
	return fmt.Errorf("%s %s: %s", name, strings.Join(args, " "), said)
}
