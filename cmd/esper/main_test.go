package main

import (
	"bufio"
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
		{"doctor", cmdDoctor, "--json", []string{"--session"}},
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
				if code := c.run([]string{ok}); code != 0 {
					t.Errorf("%s refused %q, which it takes", c.name, ok)
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
