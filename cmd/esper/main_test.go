package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"io"
	"math"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/elythi0n/esper/internal/proto"
)

func TestRunExitCodes(t *testing.T) {
	for _, tc := range []struct {
		name string
		args []string
		want int
	}{
		{"bare prints help", nil, 0},
		{"help", []string{"help"}, 0},
		{"help flag", []string{"--help"}, 0},
		{"version", []string{"version"}, 0},
		{"version flag", []string{"-v"}, 0},
		{"version json", []string{"version", "--json"}, 0},
		{"unknown command", []string{"nonsense"}, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := run(tc.args); got != tc.want {
				t.Errorf("run(%v) = %d, want %d", tc.args, got, tc.want)
			}
		})
	}
}

func TestDisplayOrDash(t *testing.T) {
	if got := displayOrDash(""); got != "—" {
		t.Errorf("empty should render as an em dash, got %q", got)
	}
	if got := displayOrDash("2026-09-01T00:00:00Z"); got != "2026-09-01T00:00:00Z" {
		t.Errorf("a stamped value should pass through, got %q", got)
	}
}

// The front door and the engine must know the same words.
//
// `esper connect` works by handing the whole command line to the engine, so
// the list of verbs to hand over is a copy of the engine's own vocabulary —
// and a copy drifts. When the engine grows a verb, this fails until somebody
// adds it here, which is the only moment anybody would think to.
//
// Skipped without a built engine: this reads the engine's own help.
func TestEveryEngineVerbIsReachableThroughTheFrontDoor(t *testing.T) {
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	binary := filepath.Join(root, "build", "esper-engine")
	if _, err := os.Stat(binary); err != nil {
		t.Skip("no built engine at build/esper-engine — run 'make build'")
	}

	out, err := exec.Command(binary, "help").CombinedOutput()
	if err != nil {
		t.Fatalf("esper-engine help: %v", err)
	}

	// Verbs the front door answers itself, or deliberately does not carry.
	ours := map[string]bool{
		"version":  true, // the control plane has its own
		"help":     true, // the control plane has its own
		"serve":    true, // the seam, spoken by the control plane and not a person
		"loopback": true, // a diagnostic of the pipeline, run against the engine
	}

	verb := regexp.MustCompile(`^  ([a-z-]+)\s{2,}`)
	scanner := bufio.NewScanner(strings.NewReader(string(out)))
	seen := 0
	for scanner.Scan() {
		m := verb.FindStringSubmatch(scanner.Text())
		if m == nil {
			continue
		}
		name := m[1]
		if ours[name] {
			continue
		}
		seen++
		if !isEngineCommand(name) {
			t.Errorf("the engine has a %q command and `esper %s` says it is unknown", name, name)
		}
	}
	if seen == 0 {
		t.Fatal("read no verbs out of the engine's help — the parser has drifted, not the lists")
	}
}

// And nothing is claimed that the engine does not have.
func TestTheFrontDoorClaimsNoVerbTheEngineLacks(t *testing.T) {
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	binary := filepath.Join(root, "build", "esper-engine")
	if _, err := os.Stat(binary); err != nil {
		t.Skip("no built engine at build/esper-engine — run 'make build'")
	}
	out, err := exec.Command(binary, "help").CombinedOutput()
	if err != nil {
		t.Fatalf("esper-engine help: %v", err)
	}
	text := string(out)
	for name := range engineCommands {
		if !strings.Contains(text, "  "+name+" ") {
			t.Errorf("`esper %s` hands off to an engine that has no %q command", name, name)
		}
	}
}

/*
A name is text somebody chose, and this column cuts it.

The bug this guards was a byte-slice: `name[:14]` on a machine called
デスクトップ cut a three-byte character in half and printed `デスクト\xe3\x83`,
which reads as a broken program rather than as a long name. Nothing here had
ever printed a name that was not ASCII, so nothing had ever seen it.
*/
func TestANameIsCutByCharactersAndSaysSo(t *testing.T) {
	for _, c := range []struct {
		name string
		in   string
		wide int
		want string
	}{
		{"a short name is left alone", "rack", 14, "rack"},
		{"a name exactly the width is left alone", "fourteen chars", 14, "fourteen chars"},
		{"a long name is cut and marked", "the machine under the stairs", 14, "the machine u…"},
		// Thirteen characters and thirty bytes: under the budget, and exactly
		// what the byte-slice this replaced cut in half.
		{"a short name in bytes it is not", "デスクトップ · مكتب", 14, "デスクトップ · مكتب"},
		{"characters, not bytes", "デスクトップコンピューターの機械", 14, "デスクトップコンピューター…"},
		{"a short key is left alone", "019a1125", 8, "019a1125"},
	} {
		t.Run(c.name, func(t *testing.T) {
			got := clip(c.in, c.wide)
			if got != c.want {
				t.Errorf("clip(%q, %d) = %q, want %q", c.in, c.wide, got, c.want)
			}
			if !utf8.ValidString(got) {
				t.Errorf("clip(%q, %d) produced bytes that are not text: %q", c.in, c.wide, got)
			}
			if n := utf8.RuneCountInString(got); n > c.wide {
				t.Errorf("clip(%q, %d) is %d characters wide", c.in, c.wide, n)
			}
		})
	}
}

/*
And the column it fills.

A device that has been unpaired since is named by its key, because the honest
answer to "who was that" is that this machine no longer knows — and either way
the column is one width, or the arrows and details after it do not line up.
*/
func TestTheNameColumnIsOneWidthWhateverIsInIt(t *testing.T) {
	for _, h := range []proto.Happening{
		{Key: "019a11258f68cdd0", Device: "rack"},
		{Key: "019a11258f68cdd0", Device: ""},
		{Key: "019a11258f68cdd0", Device: "the machine under the stairs"},
		{Key: "019a11258f68cdd0", Device: "デスクトップ"},
		{Key: "short", Device: ""},
	} {
		got := who(h)
		if n := utf8.RuneCountInString(got); n != 14 {
			t.Errorf("who(%q/%q) is %d characters and the column is 14: %q", h.Device, h.Key, n, got)
		}
		if !utf8.ValidString(got) {
			t.Errorf("who(%q/%q) is not text: %q", h.Device, h.Key, got)
		}
	}

	// A device this machine no longer knows is named by its key, not by an
	// empty column that reads as a missing line.
	if got := strings.TrimSpace(who(proto.Happening{Key: "019a11258f68cdd0"})); got != "019a1125" {
		t.Errorf("an unpaired device is named %q, and eight characters of its key is %q", got, "019a1125")
	}
}

/*
Every Esper on PATH, not just the one that wins.

`exec.LookPath` answers which binary runs. The question a diagnostic has to
answer is whether there is more than one, because that is the fault where every
other line of `esper doctor` is about a different program than the person
thinks they are running.
*/
func TestEveryBinaryOnPathIsFoundInOrder(t *testing.T) {
	first, second := t.TempDir(), t.TempDir()
	write := func(dir, name string, mode os.FileMode) string {
		t.Helper()
		p := filepath.Join(dir, name)
		if err := os.WriteFile(p, []byte("#!/bin/sh\n"), mode); err != nil {
			t.Fatalf("writing %s: %v", p, err)
		}
		return p
	}
	winner := write(first, "esper", 0o755)
	loser := write(second, "esper", 0o755)
	// Not executable: on PATH, and not something that would ever run.
	write(second, "esper-engine", 0o644)
	// A directory of the right name is not a binary either.
	if err := os.Mkdir(filepath.Join(first, "esper-broker"), 0o755); err != nil {
		t.Fatalf("making a directory in the way: %v", err)
	}

	// An empty entry means "the working directory" to some shells and nothing
	// here; either way it is not a place to look for a binary.
	t.Setenv("PATH", strings.Join([]string{first, "", second}, string(os.PathListSeparator)))

	found := otherBinariesOnPath("esper")
	if len(found) != 2 || found[0] != winner || found[1] != loser {
		t.Errorf("found %v, and PATH holds %v then %v", found, winner, loser)
	}
	if got := otherBinariesOnPath("esper-engine"); len(got) != 0 {
		t.Errorf("a file nothing can execute was reported as a binary: %v", got)
	}
	if got := otherBinariesOnPath("esper-broker"); len(got) != 0 {
		t.Errorf("a directory was reported as a binary: %v", got)
	}
	if got := otherBinariesOnPath("nothing-is-called-this"); len(got) != 0 {
		t.Errorf("found %v for a name nothing on PATH has", got)
	}
}

/*
A time the log can actually have.

`when` takes a `uint64` off the wire and hands it to `time.Unix`, which takes
an `int64` — so a number past the middle of the range is not a date in the far
future, it is a date in the distant past with the sign flipped. It says so
instead.
*/
func TestATimeThatCannotBeATimeIsSaidToBeUnknown(t *testing.T) {
	t.Setenv("TZ", "UTC")
	if got := when(1700000000); got != "2023-11-14 22:13" {
		t.Errorf("when(1700000000) = %q in UTC", got)
	}
	if got := when(0); got != "1970-01-01 00:00" {
		t.Errorf("when(0) = %q, and zero is the epoch rather than an error", got)
	}
	if got := when(math.MaxUint64); got != "—" {
		t.Errorf("when(MaxUint64) = %q, and a number no clock can hold is not a date", got)
	}
}

// toStderr runs f with stderr captured, and gives back what it wrote.
func toStderr(t *testing.T, f func()) string {
	t.Helper()
	was := os.Stderr
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("a pipe: %v", err)
	}
	os.Stderr = w
	done := make(chan string, 1)
	go func() {
		var b strings.Builder
		_, _ = io.Copy(&b, r)
		done <- b.String()
	}()
	f()
	_ = w.Close()
	os.Stderr = was
	return <-done
}

// printed runs f with stdout captured, and gives back what it wrote.
func printed(t *testing.T, f func()) string {
	t.Helper()
	was := os.Stdout
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("a pipe: %v", err)
	}
	os.Stdout = w
	done := make(chan string, 1)
	go func() {
		var b strings.Builder
		_, _ = io.Copy(&b, r)
		done <- b.String()
	}()
	f()
	_ = w.Close()
	os.Stdout = was
	return <-done
}

/*
The ladder somebody reads when nothing connects.

`esper doctor` lists the ways in, in the order they are tried, and the last
line is the one that matters: when a machine has **no** way in but an address
somebody already types, it says so. That is a working setup on a fixed address
and nothing else — and the day the machine moves, there is no rung left.

Getting the condition wrong is either a warning on a machine that is fine, or
silence on one that is about to become unreachable. Neither shows up anywhere
else, because this is a printer and nothing was reading what it printed.
*/
func TestTheWaysInAreListedAndTheDeadEndIsNamed(t *testing.T) {
	broker := "broker.example.test:7443"

	full := printed(t, func() {
		reachTiers(&proto.Status{MDNS: true, Overlays: []string{"wg0", "tailscale0"}, Broker: &broker})
	})
	for _, rung := range []string{"given", "lan", "remembered", "overlay", "broker"} {
		if !strings.Contains(full, rung) {
			t.Errorf("a machine with every way in does not list %q:\n%s", rung, full)
		}
	}
	if !strings.Contains(full, broker) {
		t.Errorf("a configured broker is not named:\n%s", full)
	}
	if !strings.Contains(full, "wg0") || !strings.Contains(full, "tailscale0") {
		t.Errorf("the overlays this machine has are not named:\n%s", full)
	}
	if strings.Contains(full, "the only way in is an address somebody already knows") {
		t.Errorf("a machine with every way in was warned it had none:\n%s", full)
	}

	bare := printed(t, func() { reachTiers(&proto.Status{}) })
	if !strings.Contains(bare, "the only way in is an address somebody already knows") {
		t.Errorf("a machine reachable only by a typed address was not told so:\n%s", bare)
	}
	// And it says what is missing rather than only that something is.
	for _, why := range []string{"discovery.mdns is off", "no WireGuard", "none configured"} {
		if !strings.Contains(bare, why) {
			t.Errorf("a rung is unavailable and the reason %q is not given:\n%s", why, bare)
		}
	}

	// One rung is enough to not be a dead end — checked for each in turn,
	// because a condition made of three `&&`s has three ways to be wrong.
	for _, one := range []struct {
		name   string
		status proto.Status
	}{
		{"the network can be asked", proto.Status{MDNS: true}},
		{"an overlay is up", proto.Status{Overlays: []string{"wg0"}}},
		{"a broker is configured", proto.Status{Broker: &broker}},
	} {
		out := printed(t, func() { reachTiers(&one.status) })
		if strings.Contains(out, "the only way in is an address somebody already knows") {
			t.Errorf("%s, and it was still called a dead end:\n%s", one.name, out)
		}
	}

	// `remembered` belongs to the controller's store, and a host cannot know
	// whether somebody else remembers it — so it is never a tick.
	if strings.Contains(full, "✓ remembered") {
		t.Errorf("this machine claimed to know that somebody remembers it:\n%s", full)
	}
}

// A capability line says which way it went, and reports it back so the caller
// can count what is missing.
func TestACapabilityLineSaysWhichWayItWent(t *testing.T) {
	yes := printed(t, func() {
		if !capability("capture", proto.Capability{Available: true, Detail: "x11"}) {
			t.Error("an available capability reported itself as missing")
		}
	})
	if !strings.Contains(yes, "capture") || !strings.Contains(yes, "x11") || !strings.Contains(yes, "✓") {
		t.Errorf("an available capability printed %q", yes)
	}

	no := printed(t, func() {
		if capability("input", proto.Capability{Available: false, Detail: "no uinput"}) {
			t.Error("a missing capability reported itself as available")
		}
	})
	if !strings.Contains(no, "✗") || !strings.Contains(no, "no uinput") {
		t.Errorf("a missing capability printed %q, and the detail is why", no)
	}
}

// A missing capability says what was tried, one attempt per line, and none of
// those lines runs past a terminal's width.
//
// The attempts are the half somebody acts on: "no input backend" is a fault,
// and "the RemoteDesktop portal is not installed" is an errand.
func TestAMissingCapabilitySaysWhatWasTried(t *testing.T) {
	t.Setenv("COLUMNS", "80")
	cap := proto.Capability{
		Available: false,
		Detail:    "nothing — this machine is view only",
		Attempts: []string{
			"x11 xtest: no input backend: X11 via DISPLAY: Failed to parse value ''",
			"wayland-portal: no input backend: the RemoteDesktop portal refused CreateSession: org.freedesktop.DBus.Error.UnknownMethod — No such interface on object at path /org/freedesktop/portal/desktop",
		},
	}
	out := printed(t, func() { capability("input   ", cap) })

	for _, attempt := range cap.Attempts {
		first, _, _ := strings.Cut(attempt, " ")
		if !strings.Contains(out, first) {
			t.Errorf("the attempt beginning %q is not in the report:\n%s", first, out)
		}
	}
	for _, line := range strings.Split(strings.TrimRight(out, "\n"), "\n") {
		if n := utf8.RuneCountInString(line); n > 80 {
			t.Errorf("a line is %d columns wide, which a terminal folds where it likes:\n%s", n, line)
		}
	}
	// Every line after the first sits under the detail, so the block reads as
	// one answer rather than as new report lines.
	lines := strings.Split(strings.TrimRight(out, "\n"), "\n")
	if len(lines) < 3 {
		t.Fatalf("two attempts folded to %d line(s):\n%s", len(lines), out)
	}
	for _, line := range lines[1:] {
		if !strings.HasPrefix(line, "           ") {
			t.Errorf("a continuation line is not indented under the detail: %q", line)
		}
	}
}

// A capability that works keeps its rejected alternatives to itself: they are
// noise beside a backend that is in use.
func TestAWorkingCapabilityDoesNotListWhatItRejected(t *testing.T) {
	out := printed(t, func() {
		capability("input   ", proto.Capability{
			Available: true,
			Detail:    "x11-xtest",
			Attempts:  []string{"wayland-wlr: could not find wayland compositor", "x11 xtest: in use"},
		})
	})
	if strings.Contains(out, "wayland-wlr") {
		t.Errorf("a working capability listed a backend it never needed:\n%s", out)
	}
	if !strings.Contains(out, "x11-xtest") {
		t.Errorf("a working capability did not say which backend it is using:\n%s", out)
	}
}

// Folding breaks on spaces, and leaves a word longer than the whole width
// alone: half a path helps nobody.
func TestFoldingLeavesAnUnbreakableWordWhole(t *testing.T) {
	long := "/a/very/long/path/that/has/no/spaces/in/it/at/all/and/keeps/going/for/a/while"
	got := wrapped("looked in "+long+" and found nothing", 30)
	found := false
	for _, line := range got {
		if line == long {
			found = true
		}
	}
	if !found {
		t.Errorf("the long word was broken up: %q", got)
	}
	for _, line := range got {
		if strings.Contains(line, " ") && utf8.RuneCountInString(line) > 30 {
			t.Errorf("a foldable line is %d wide: %q", utf8.RuneCountInString(line), line)
		}
	}
}

// No broker is the ordinary answer — Esper ships no default anywhere — so it
// is an empty string rather than a word somebody might read as an address.
func TestNoBrokerIsNothingRatherThanAName(t *testing.T) {
	if got := brokerOf(&proto.Status{}); got != "" {
		t.Errorf("a machine with no broker reported %q", got)
	}
	broker := "broker.example.test:7443"
	if got := brokerOf(&proto.Status{Broker: &broker}); got != broker {
		t.Errorf("a configured broker reported %q", got)
	}
}

/*
Asking about a command is asking, not attempting.

`--help` after a command used to reach the command itself, and each one did
something different with it: `doctor` ran the diagnostic, `sessions` answered
`"--help" is not a number of lines`, and `version` printed the version. Three
ways of telling somebody that asking was a mistake — the same fault the engine's
dispatcher already names in a comment, in the binary people actually type.

What is held here is the shape of the answer, not its wording: a usage line
naming `esper` and the command, and no exit code but nought.
*/
func TestEveryVerbThisProgramOwnsAnswersWhenAsked(t *testing.T) {
	mine := []string{"api", "doctor", "sessions", "version"}
	for _, command := range mine {
		t.Run(command, func(t *testing.T) {
			usage, ours := usageFor(command)
			if !ours {
				t.Fatalf("%s is dispatched here and answers no usage of its own", command)
			}
			if !strings.HasPrefix(usage, "usage: esper "+command) {
				t.Errorf("the usage does not begin by naming the command somebody typed:\n%s", usage)
			}
			// Long enough to say something. A usage line on its own is a
			// signature, not an answer to "what does this do".
			if len(strings.Split(strings.TrimSpace(usage), "\n")) < 3 {
				t.Errorf("%s answers with a signature and nothing else:\n%s", command, usage)
			}
		})
	}

	// And the verbs that are handed over answer for themselves, under the name
	// they were reached by — so this must NOT claim them.
	for _, theirs := range []string{"host", "broker", "discover", "pair", "cp", "grant"} {
		if _, ours := usageFor(theirs); ours {
			t.Errorf("%q is handed to another binary and this one answers for it too", theirs)
		}
	}
}

// Every verb the dispatcher owns has a usage, and no verb has one it cannot
// reach — the same pairing `check-help.py` holds the engine to, for the half
// of the front door that is written in Go.
func TestNoVerbIsOwnedWithoutAnAnswerOrAnsweredWithoutBeingOwned(t *testing.T) {
	owned := []string{"doctor", "sessions", "api"}
	for _, command := range owned {
		if _, ours := usageFor(command); !ours {
			t.Errorf("`esper %s` is run by this program and cannot be asked about", command)
		}
	}
	if _, ours := usageFor("telepathy"); ours {
		t.Error("a usage came back for a command this program does not have")
	}
}

/*
A flag this program does not know is refused, not ignored.

`doctor` ran the ordinary diagnostic for anything it did not recognise, and
`version` printed the version. Both are quiet failures of the worst kind:
somebody who asked for a deeper check by a name this does not have got the
shallow one and no sign of it — and the whole point of `doctor` is to be
believed about what it checked.

`api` and `sessions` already refused, which is what made the other two look
like oversights rather than a decision.
*/
func TestAFlagThisDoesNotKnowIsRefused(t *testing.T) {
	for _, c := range []struct {
		name string
		run  func([]string) int
		bad  string
		good []string
	}{
		{"doctor", cmdDoctor, "--porcelain", []string{"--session", "--json"}},
		{"version", cmdVersion, "--porcelain", []string{"--json"}},
	} {
		t.Run(c.name, func(t *testing.T) {
			/*
			   Read from what it *said*, not from the code it returned.

			   The first version of this test asserted a non-zero exit — and
			   `doctor` exits non-zero anyway on a machine with no engine, which
			   is every machine a unit test runs on. It passed with the refusal
			   removed. What separates "refused the flag" from "ran and failed
			   for its own reasons" is the sentence.
			*/
			var code int
			said := toStderr(t, func() { code = c.run([]string{c.bad}) })
			if !strings.Contains(said, c.bad) {
				t.Errorf("%s did not say it could not use %q; it said: %s", c.name, c.bad, said)
			}
			if code == 0 {
				t.Errorf("%s accepted %q and answered success", c.name, c.bad)
			}
			// And the flag it does know is still a flag it takes: a refusal
			// that refuses everything is not a refusal, it is a broken command.
			for _, ok := range c.good {
				if ok == "--session" {
					// Holds a real session: too slow for a unit test, and the
					// usage below is what says it exists.
					continue
				}
				// Read from what it said here too. `doctor` exits non-zero on
				// a machine with no engine beside it, which is every machine a
				// unit test runs on — so the exit code cannot tell "refused
				// the flag" from "ran and found nothing".
				said := toStderr(t, func() { _ = c.run([]string{ok}) })
				if strings.Contains(said, "unknown option") {
					t.Errorf("%s refused %q, which it takes: %s", c.name, ok, said)
				}
			}
		})
	}
}

// The usage a refusal prints is the usage `--help` prints.
//
// Two answers to one question is how a flag ends up documented in one place
// and refused in the other — which is exactly the state `--session` was in:
// named in the command's own closing lines, and absent from any usage.
func TestTheUsageOnAnErrorIsTheUsageOnRequest(t *testing.T) {
	for _, c := range []struct {
		command string
		usage   func() string
		mention string
	}{
		{"doctor", doctorUsage, "--session"},
		{"version", versionUsage, "--json"},
	} {
		asked, ours := usageFor(c.command)
		if !ours {
			t.Fatalf("%s answers no usage", c.command)
		}
		if asked != c.usage() {
			t.Errorf("`esper %s --help` and its refusal print different text", c.command)
		}
		if !strings.Contains(asked, c.mention) {
			t.Errorf("`esper %s` takes %s and its usage does not mention it:\n%s",
				c.command, c.mention, asked)
		}
	}
}

// builtEsper is the front door, skipped where it has not been built.
//
// A real process, because what is being checked is the *streams*: an engine
// line on standard output is only visible from outside the program.
func builtEsper(t *testing.T) string {
	t.Helper()
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	p := filepath.Join(root, "build", "esper")
	if _, err := os.Stat(p); err != nil {
		t.Skip("no built front door at build/esper — run 'make build'")
	}
	return p
}

// runWithHome runs it against a home of this test's own, and separates the
// streams — which is the whole point of the first test below.
func runWithHome(t *testing.T, esper, home string, args ...string) (out, errOut string, code int) {
	t.Helper()
	cmd := exec.Command(esper, args...) // #nosec G204 -- the binary is this repository's own build
	cmd.Env = append(os.Environ(), "ESPER_HOME="+home)
	var stdout, stderr strings.Builder
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := cmd.Run()
	var exited *exec.ExitError
	if errors.As(err, &exited) {
		code = exited.ExitCode()
	} else if err != nil {
		t.Fatalf("running %v: %v", args, err)
	}
	return stdout.String(), stderr.String(), code
}

/*
`sessions --json` is one object a program can read, on a clean standard output.

# Why both halves matter

The trail is the one record this product keeps, and it was the only one with no
machine-readable form — `devices`, `identity`, `config` and `version` all answer
`--json`. Anything auditing what happened had to parse a table laid out for a
person to read.

The second half is what makes the first usable: the engine writes its own lines
while this runs, and a single "engine started" on standard output makes the
answer unparseable. That is why the supervisor relays rather than inherits, and
this is what says so.
*/
func TestSessionsAsJSONIsOneObjectOnACleanStandardOutput(t *testing.T) {
	esper := builtEsper(t)
	home := t.TempDir()

	out, errOut, code := runWithHome(t, esper, home, "sessions", "--json")
	if code != 0 {
		t.Fatalf("sessions --json exited %d: %s", code, errOut)
	}
	var answer struct {
		Asked      uint32 `json:"asked"`
		Count      int    `json:"count"`
		Happenings []struct {
			At    uint64 `json:"at"`
			Event string `json:"event"`
		} `json:"happenings"`
	}
	if err := json.Unmarshal([]byte(out), &answer); err != nil {
		t.Fatalf("standard output is not one object: %v\n%s", err, out)
	}
	if answer.Asked != 20 {
		t.Errorf("the default is 20 lines and the answer says %d", answer.Asked)
	}
	// **An empty list, never null.** A caller looping over `happenings` on a
	// machine where nothing has happened should get an empty loop rather than
	// a crash. Nothing in the writer guards this — `Sessions` refuses an
	// answer that is not a list — so this is where the property lives.
	if answer.Happenings == nil {
		t.Error("happenings came back as null, so a caller looping over it crashes on a quiet machine")
	}
	if !strings.Contains(out, `"happenings": []`) {
		t.Errorf("an empty trail is not written as an empty list:\n%s", out)
	}
	// And the engine's own lines went somewhere else.
	if strings.Contains(out, "engine started") {
		t.Errorf("the engine's log reached standard output, so the answer cannot be parsed:\n%s", out)
	}

	// A count that was asked for travels too, so a caller can tell "there were
	// only three" from "the answer was cut".
	out, errOut, code = runWithHome(t, esper, home, "sessions", "5", "--json")
	if code != 0 {
		t.Fatalf("sessions 5 --json exited %d: %s", code, errOut)
	}
	if err := json.Unmarshal([]byte(out), &answer); err != nil {
		t.Fatalf("standard output is not one object: %v\n%s", err, out)
	}
	if answer.Asked != 5 {
		t.Errorf("asked for 5 lines and the answer says %d", answer.Asked)
	}
}

// And an option this command does not have is still named as one.
func TestSessionsStillNamesAnUnknownOption(t *testing.T) {
	esper := builtEsper(t)
	home := t.TempDir()

	_, errOut, code := runWithHome(t, esper, home, "sessions", "--nope")
	if code == 0 {
		t.Fatal("an unknown option was accepted")
	}
	if !strings.Contains(errOut, `unknown option "--nope"`) {
		t.Errorf("it was not named as an option: %s", errOut)
	}
	// A bad *number* is still the other kind of mistake.
	_, errOut, code = runWithHome(t, esper, home, "sessions", "many")
	if code == 0 {
		t.Fatal("a number that is not one was accepted")
	}
	if !strings.Contains(errOut, "is not a number of lines") {
		t.Errorf("a bad number was reported as something else: %s", errOut)
	}
}

/*
`esper sessions` says where the whole record is.

Twenty lines is a default and the file holds months, so the answer to "show me
everything that device ever did" is `grep` — which needs a path this command
never printed. Somebody had to know that Esper keeps a `sessions.log`, and
where its home directory is, to ask the record anything the default did not
already show.

Not printed where there is no file: a machine with nothing recorded is told
that, and a second sentence about a file that does not exist is noise.
*/
func TestSessionsSaysWhereTheWholeRecordIs(t *testing.T) {
	esper := builtEsper(t)
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	fixture := filepath.Join(root, "testdata", "fixtures", "session_log_screens.jsonl")
	lines, err := os.ReadFile(fixture)
	if err != nil {
		t.Skipf("no session log fixture to read: %v", err)
	}

	home := t.TempDir()
	trail := filepath.Join(home, "sessions.log")
	if err := os.WriteFile(trail, lines, 0o600); err != nil {
		t.Fatalf("writing a trail: %v", err)
	}

	out, errOut, code := runWithHome(t, esper, home, "sessions")
	if code != 0 {
		t.Fatalf("sessions exited %d: %s", code, errOut)
	}
	if !strings.Contains(out, trail) {
		t.Errorf("the record's path is not named, so nothing says what to grep:\n%s", out)
	}
	// And what is in it, because a path with no shape is a path somebody opens
	// in the wrong tool.
	if !strings.Contains(out, "JSON") {
		t.Errorf("the path is named and its shape is not: %s", out)
	}

	// A machine with nothing recorded is told that, and nothing else.
	empty := t.TempDir()
	out, _, code = runWithHome(t, esper, empty, "sessions")
	if code != 0 {
		t.Fatalf("sessions on an empty home exited %d", code)
	}
	if strings.Contains(out, "sessions.log") {
		t.Errorf("a file that does not exist was named anyway:\n%s", out)
	}
	if !strings.Contains(out, "nothing has happened") {
		t.Errorf("an empty trail does not say so: %s", out)
	}
}

/*
`version --json` is JSON whatever was stamped into the binary.

Go's `%q` is Go's quoting, not JSON's: a byte with no short escape comes out as
`\x01`, and every JSON parser rejects that escape. The two strings here arrive
from `-ldflags` at link time, so the one output meant for a script became text
no script could read as soon as a tag was unusual — and nothing would have said
so, because the human output was still fine.
*/
func TestVersionAsJSONSurvivesAnAwkwardStamp(t *testing.T) {
	was := version
	t.Cleanup(func() { version = was })

	for _, stamped := range []string{
		"v1.2.3",
		"v1.0\x01dev",    // a control byte: `%q` writes \x01
		`v1.0"quoted`,    // a quote
		`v1.0\backslash`, // a backslash
		"v1.0–en-dash",   // not ASCII
	} {
		version = stamped
		out := printed(t, func() { _ = cmdVersion([]string{"--json"}) })
		var answer struct {
			Version  string `json:"version"`
			Protocol int    `json:"protocol"`
		}
		if err := json.Unmarshal([]byte(out), &answer); err != nil {
			t.Fatalf("a version of %q produced output no parser reads: %v\n%s", stamped, err, out)
		}
		if answer.Version != stamped {
			t.Errorf("a version of %q came back as %q", stamped, answer.Version)
		}
	}
}

/*
`esper doctor` names this machine's state directory, and says when the
environment chose it.

"My devices have disappeared" is almost always two directories: the state lives
in `$ESPER_HOME` where that is set and beside the user's config where it is
not, and both are invisible. A shell with the variable exported, a service unit
that sets it, a `sudo` that does not carry it — the same command on the same
machine then answers about a different store, and nothing on the diagnostic
said which one.

Driven through the real binary, because the environment is the thing under
test and a function called in-process would be reading this test's own.
*/
func TestDoctorNamesTheStateDirectory(t *testing.T) {
	esper := builtEsper(t)
	home := t.TempDir()

	out, errOut, _ := runWithHome(t, esper, home, "doctor")
	if !strings.Contains(out, home) {
		t.Errorf("the state directory is not named:\n%s\n%s", out, errOut)
	}
	// And that the environment is where it came from, because that is the
	// case somebody is surprised by.
	if !strings.Contains(out, "ESPER_HOME") {
		t.Errorf("a directory chosen by the environment does not say so:\n%s", out)
	}

	// Without it, the directory is still named and the environment is not
	// blamed for a choice it did not make.
	cmd := exec.Command(esper, "doctor") // #nosec G204 -- this repository's own build
	cmd.Env = []string{"PATH=" + os.Getenv("PATH"), "HOME=" + home}
	var stdout strings.Builder
	cmd.Stdout = &stdout
	_ = cmd.Run()
	plain := stdout.String()
	if !strings.Contains(plain, "state") {
		t.Errorf("no state line at all without the variable:\n%s", plain)
	}
	if strings.Contains(plain, "(from ESPER_HOME)") {
		t.Errorf("the environment was credited for a directory it did not choose:\n%s", plain)
	}
}
