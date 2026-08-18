//go:build linux

package service

import (
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

/*
What is tested here, and what deliberately is not.

`Enable` and `Disable` drive the real `systemctl --user`, which is the session
of whoever is running the tests: a test that called them would enable or
disable the host on a developer's own machine, and a test suite that changes
the machine it runs on is worse than no suite. So what is tested is everything
either of them does *before* it reaches systemd — which is where the promises
are anyway: the unit is written, a unit somebody else wrote is left alone, and
the file itself is one systemd will accept.

`installUnit` and `unitDir` read `XDG_CONFIG_HOME`, so every test here works in
a directory of its own and nothing touches `~/.config/systemd/user`.
*/

func TestTheUnitIsWrittenWhenThereIsNone(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", dir)

	said := &strings.Builder{}
	path, err := installUnit(Report{To: said})
	if err != nil {
		t.Fatalf("installing into an empty directory: %v", err)
	}

	want := filepath.Join(dir, "systemd", "user", unitName)
	if path != want {
		t.Errorf("wrote %s, and the unit belongs at %s", path, want)
	}
	written, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading back what was written: %v", err)
	}
	if string(written) != Unit {
		t.Error("what was written is not the unit this binary carries")
	}

	// The unit names the state directory and the hardening; a mode anybody on
	// the machine can write is a mode anybody can change what the host does.
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0o600 {
		t.Errorf("the unit is %04o, and it is written 0600", perm)
	}
}

/*
The promise in the package's own doc comment, and the only one here that
protects somebody else's work.

A distribution package may have installed a unit with different paths and
different hardening. Overwriting it silently would undo their packaging, and
the person would find out when the host next started with settings they did
not choose.
*/
func TestAUnitThisDidNotWriteIsKept(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", dir)

	path := filepath.Join(dir, "systemd", "user", unitName)
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatalf("making the unit directory: %v", err)
	}
	theirs := "[Unit]\nDescription=Esper, as this distribution packages it\n"
	if err := os.WriteFile(path, []byte(theirs), 0o644); err != nil {
		t.Fatalf("writing somebody else's unit: %v", err)
	}

	said := &strings.Builder{}
	if _, err := installUnit(Report{To: said}); err != nil {
		t.Fatalf("installing over somebody else's unit: %v", err)
	}

	after, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading the unit back: %v", err)
	}
	if string(after) != theirs {
		t.Fatal("a unit this package did not write was overwritten, which is the one thing it promises not to do")
	}
	if !strings.Contains(said.String(), "kept") {
		t.Errorf("it kept the unit and did not say so — the report read %q", said.String())
	}
}

func TestAnIdenticalUnitIsLeftAloneAndSaidToBeUnchanged(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", dir)

	path := filepath.Join(dir, "systemd", "user", unitName)
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatalf("making the unit directory: %v", err)
	}
	if err := os.WriteFile(path, []byte(Unit), 0o600); err != nil {
		t.Fatalf("writing the unit: %v", err)
	}

	said := &strings.Builder{}
	if _, err := installUnit(Report{To: said}); err != nil {
		t.Fatalf("installing the same unit again: %v", err)
	}
	if !strings.Contains(said.String(), "unchanged") {
		t.Errorf("the same unit was installed again and not reported as unchanged — %q", said.String())
	}
	// "Unchanged" and "kept" are two different things to be told, and telling
	// somebody their unit was kept when it is this one's own is a lie about
	// whose file it is.
	if strings.Contains(said.String(), "kept") {
		t.Errorf("its own unit was reported as somebody else's — %q", said.String())
	}
}

func TestTheUnitDirectoryFollowsTheConfigHome(t *testing.T) {
	t.Setenv("XDG_CONFIG_HOME", "/somewhere/else")
	if got, want := unitDir(), filepath.Join("/somewhere/else", "systemd", "user"); got != want {
		t.Errorf("unitDir is %s and XDG_CONFIG_HOME says %s", got, want)
	}

	// Unset, not empty-and-ignored: an empty XDG_CONFIG_HOME is the same as
	// none, which is what the specification says and what the code does.
	t.Setenv("XDG_CONFIG_HOME", "")
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skip("no home directory to fall back to")
	}
	if got, want := unitDir(), filepath.Join(home, ".config", "systemd", "user"); got != want {
		t.Errorf("with no XDG_CONFIG_HOME unitDir is %s, and the fallback is %s", got, want)
	}
}

// A report is where the wrapper says what it is doing, and nothing about a
// service change should depend on somebody listening.
func TestAReportWithNowhereToWriteChangesNothing(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", dir)

	path, err := installUnit(Report{})
	if err != nil {
		t.Fatalf("installing with no report: %v", err)
	}
	written, err := os.ReadFile(path)
	if err != nil || string(written) != Unit {
		t.Fatal("the unit was not written when there was nobody to tell about it")
	}
}

/*
The unit itself, which is the only copy there is.

It lives in this package rather than in `packaging/` precisely so the file the
binary writes and the file a distribution ships cannot drift apart — so it is
worth holding to more than "it is a string".
*/
/*
The unit file, read the way systemd reads it.

Parsed rather than searched: the file's own comments name sections and keys —
the block above the restart limits says the words "[Service]" while explaining
why they are not in it — so a test that looks for a string finds the
explanation and draws the opposite conclusion. It did, the first time this was
written. Comments are dropped and each key is recorded under the section
header it actually falls beneath.
*/
func unitKeys(t *testing.T) map[string]string {
	t.Helper()
	keys := map[string]string{}
	section := ""
	for _, line := range strings.Split(Unit, "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") || strings.HasPrefix(line, ";") {
			continue
		}
		if strings.HasPrefix(line, "[") && strings.HasSuffix(line, "]") {
			section = line
			continue
		}
		name, value, found := strings.Cut(line, "=")
		if !found {
			t.Errorf("a line in %s that is neither a section nor a setting: %q", section, line)
			continue
		}
		keys[section+" "+strings.TrimSpace(name)] = strings.TrimSpace(value)
	}
	return keys
}

/*
The unit itself, which is the only copy there is.

It lives in this package rather than in `packaging/` precisely so the file the
binary writes and the file a distribution ships cannot drift apart — so it is
worth holding to more than "it is a string".
*/
func TestTheEmbeddedUnitIsOneSystemdWouldAccept(t *testing.T) {
	keys := unitKeys(t)

	/*
	   The restart rate-limit belongs in [Unit].

	   systemd ignores `StartLimitIntervalSec` and `StartLimitBurst` **silently**
	   in [Service], which is where they read as though they belong. That
	   happened once and `systemd-analyze verify` is what caught it; a reading of
	   the file would not have. Without them a binary that cannot start spins for
	   ever and the journal is one line ten thousand times instead of the error.
	*/
	for _, key := range []string{"StartLimitIntervalSec", "StartLimitBurst"} {
		if _, ok := keys["[Unit] "+key]; ok {
			continue
		}
		if _, ok := keys["[Service] "+key]; ok {
			t.Errorf("%s is in [Service], where systemd ignores it without saying so", key)
			continue
		}
		t.Errorf("the unit sets no %s, so a binary that cannot start spins for ever", key)
	}

	// What the service is, and what starts it.
	if got := keys["[Service] ExecStart"]; !strings.HasSuffix(got, "esper host") {
		t.Errorf("ExecStart is %q, and the service exists to run `esper host`", got)
	}
	if got := keys["[Service] Restart"]; got != "on-failure" {
		// A deliberate shutdown should stay shut down; a crash should come back.
		t.Errorf("Restart is %q, and it is on-failure", got)
	}
	if got := keys["[Install] WantedBy"]; got != "default.target" {
		t.Errorf("WantedBy is %q, so `systemctl --user enable` has nothing to hook it to", got)
	}

	// And the hardening the package's doc comment promises, since a line
	// removed from it is a line nothing else would miss.
	for key, want := range map[string]string{
		"[Service] NoNewPrivileges": "true",
		"[Service] ProtectSystem":   "strict",
		"[Service] UMask":           "0077",
		"[Service] ProtectHome":     "read-only",
	} {
		if got := keys[key]; got != want {
			t.Errorf("%s is %q and the hardening says %q", key, got, want)
		}
	}

	// The one path it may write: its own state. Without this line every write
	// the engine makes is refused by ProtectSystem, and the failure is at
	// runtime rather than here.
	if got := keys["[Service] ReadWritePaths"]; !strings.Contains(got, ".esper") {
		t.Errorf("ReadWritePaths is %q, and the engine writes its identity key and device store under ~/.esper", got)
	}
}

/*
And systemd's own opinion of it, where systemd is installed.

Skipped rather than failed where it is not: this is the one assertion here that
needs the thing being wrapped, and a machine without it is a machine where the
rest of this file still means what it says.
*/
func TestSystemdVerifiesTheEmbeddedUnit(t *testing.T) {
	analyze, err := exec.LookPath("systemd-analyze")
	if err != nil {
		t.Skip("no systemd-analyze on this machine")
	}

	dir := t.TempDir()
	path := filepath.Join(dir, unitName)
	if err := os.WriteFile(path, []byte(Unit), 0o600); err != nil {
		t.Fatalf("writing the unit out to verify: %v", err)
	}

	/*
	   A home with the binary in it, because `%h` is resolved from `HOME` and
	   systemd checks that `ExecStart` names something it could actually run.

	   Without this the verdict is always "Command … is not executable" on any
	   machine where Esper is not installed — which is every machine the tests
	   run on — and a check whose output has to be filtered before it means
	   anything is a check that stops meaning anything. Given a home, the
	   verdict covers the ExecStart path as well as the sections.
	*/
	home := t.TempDir()
	bin := filepath.Join(home, ".local", "bin")
	if err := os.MkdirAll(bin, 0o700); err != nil {
		t.Fatalf("making a home for the unit to point at: %v", err)
	}
	if err := os.WriteFile(filepath.Join(bin, "esper"), []byte("#!/bin/sh\n"), 0o700); err != nil {
		t.Fatalf("writing the binary the unit starts: %v", err)
	}

	// `--user`, because it is a user unit and the system manager's rules are
	// not the ones it will be read under.
	cmd := exec.Command(analyze, "verify", "--user", path) // #nosec G204 -- a looked-up path and a temp file
	cmd.Env = append(os.Environ(), "HOME="+home)
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Errorf("systemd will not accept the unit this binary installs:\n%s", out)
	}
}

/*
What the machine says about the host, and the one thing that must hold whatever
it says.

The answer depends on the machine the tests run on — systemd may be absent, the
unit may be installed or not — so what is asserted is the report's own
consistency: `Managed` is exactly the case where there was somebody to ask, and
a state that is not managed has to say why, because a screen showing three
switches and no reason is a screen nobody can act on.
*/
func TestWhatTheManagerSaysIsSelfConsistent(t *testing.T) {
	state := Now()
	if state.Managed && state.Why != "" {
		t.Errorf("managed, and still explaining why not: %q", state.Why)
	}
	if !state.Managed {
		if state.Why == "" {
			t.Error("unmanaged and silent about it, so a screen has nothing to show")
		}
		if state.Enabled || state.Active {
			t.Error("nothing here answers, and it reported the host as enabled or active anyway")
		}
	}
}

/*
With no systemctl to ask, everything refuses — and refuses *first*.

The package promises it refuses rather than guesses, and the order matters as
much as the refusal: a wrapper that wrote the unit and then discovered it had
no service manager would leave a file nobody installed and no way to tell that
from a real install. Emptying `PATH` is how that is reached without a machine
that genuinely has no systemd.
*/
func TestWithNoSystemctlEverythingRefusesAndWritesNothing(t *testing.T) {
	config := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", config)
	t.Setenv("PATH", t.TempDir())

	said := &strings.Builder{}
	for _, call := range []struct {
		name string
		fn   func(Report) error
	}{{"Enable", Enable}, {"Disable", Disable}} {
		err := call.fn(Report{To: said})
		if err == nil {
			t.Fatalf("%s found no systemctl and did not say so", call.name)
		}
		if !errors.Is(err, ErrUnsupported) {
			t.Errorf("%s refused with %v, which is not ErrUnsupported and so nothing can match on it", call.name, err)
		}
		// The message has to name what this machine would need instead, or
		// somebody is left with a refusal and no next step.
		if !strings.Contains(err.Error(), "esper host") {
			t.Errorf("%s refused without naming the command to run: %v", call.name, err)
		}
	}

	if _, err := os.Stat(filepath.Join(config, "systemd", "user", unitName)); !errors.Is(err, os.ErrNotExist) {
		t.Error("a unit was written on a machine with no service manager to load it")
	}

	state := Now()
	if state.Managed {
		t.Error("nothing to ask, and it reported the host as managed")
	}
	if !strings.Contains(state.Why, "systemd") {
		t.Errorf("unmanaged, and the reason does not name what is missing: %q", state.Why)
	}
}

/*
What a command said is what the error says.

`run` reports the command before running it — that is the whole point of the
wrapper, so a machine where it fails is one somebody can finish by hand — and
when the command fails it puts *the command's own words* in the error. A
wrapper that returned "exit status 1" would throw away the only sentence that
says what went wrong.
*/
func TestACommandsOwnWordsSurviveTheWrapper(t *testing.T) {
	said := &strings.Builder{}
	if err := run(Report{To: said}, "sh", "-c", "exit 0"); err != nil {
		t.Fatalf("a command that succeeds: %v", err)
	}
	if !strings.Contains(said.String(), "running") || !strings.Contains(said.String(), "sh -c exit 0") {
		t.Errorf("the command was run and not reported first — %q", said.String())
	}

	err := run(Report{To: said}, "sh", "-c", "echo 'Failed to enable unit: Unit is masked.' >&2; exit 1")
	if err == nil {
		t.Fatal("a command that failed was reported as success")
	}
	if !strings.Contains(err.Error(), "Unit is masked") {
		t.Errorf("the error is %q, and the command said why", err)
	}

	// A command that fails silently has no words to pass on, and then the
	// exit status is all there is — it must still name the command.
	silent := run(Report{To: said}, "sh", "-c", "exit 3")
	if silent == nil || !strings.Contains(silent.Error(), "sh -c") {
		t.Errorf("a silent failure lost the command it was: %v", silent)
	}
}
