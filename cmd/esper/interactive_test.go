package main

import (
	"bytes"
	"encoding/json"
	"os"
	"strings"
	"testing"

	"golang.org/x/sys/unix"

	"github.com/elythi0n/esper/internal/devices"
)

// aHomeWith writes a device store and points ESPER_HOME at it.
func aHomeWith(t *testing.T, paired []devices.Device) string {
	t.Helper()
	home := t.TempDir()
	// The store's own shape: a version and devices keyed by their public key,
	// which is what `Load` reads and what a wrong guess here would hide.
	keyed := map[string]devices.Device{}
	for _, d := range paired {
		keyed[d.PublicKey.String()] = d
	}
	raw, err := json.Marshal(map[string]any{"version": devices.Version, "devices": keyed})
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	if err := os.WriteFile(devices.PathIn(home), raw, 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}
	// A store this test could not read back is a test measuring its own
	// fixture, so it is read before anything depends on it.
	back, err := devices.Load(devices.PathIn(home))
	if err != nil {
		t.Fatalf("the fixture is not a store: %v", err)
	}
	if len(back) != len(paired) {
		t.Fatalf("wrote %d devices and read %d back", len(paired), len(back))
	}
	t.Setenv("ESPER_HOME", home)
	return home
}

func aDevice(name string, key byte) devices.Device {
	var k devices.Key
	for i := range k {
		k[i] = key
	}
	return devices.Device{PublicKey: k, Name: name, Permissions: []devices.Permission{"view"}}
}

/*
With no terminal, `esper` is what it always was.

The distinction is **stdin**, not stdout: `esper | less` is still somebody
sitting there, and a script that runs this with no arguments and no terminal
needs the help rather than a question it cannot answer.
*/
func TestWithNobodyThereItPrintsTheHelp(t *testing.T) {
	aHomeWith(t, []devices.Device{aDevice("elythion", 1)})
	was := atATerminal
	t.Cleanup(func() { atATerminal = was })
	atATerminal = func() bool { return false }

	var out bytes.Buffer
	chosen, handled := interactive(strings.NewReader("1\n"), &out)
	if handled {
		t.Fatalf("it asked a question with nobody there: %q", out.String())
	}
	if chosen != nil || out.Len() != 0 {
		t.Fatalf("it wrote %q and chose %v", out.String(), chosen)
	}
}

// And with nobody paired there is nothing to offer, so the help says `pair`.
func TestWithNothingPairedItPrintsTheHelp(t *testing.T) {
	aHomeWith(t, nil)
	was := atATerminal
	t.Cleanup(func() { atATerminal = was })
	atATerminal = func() bool { return true }

	var out bytes.Buffer
	_, handled := interactive(strings.NewReader("1\n"), &out)
	if handled {
		t.Fatalf("a machine with nothing paired offered a list: %q", out.String())
	}
}

// The ordinary way out, and the one somebody presses by accident.
func TestQuittingAndEndOfInput(t *testing.T) {
	aHomeWith(t, []devices.Device{aDevice("elythion", 1), aDevice("workshop", 2)})
	was := atATerminal
	t.Cleanup(func() { atATerminal = was })
	atATerminal = func() bool { return true }

	for _, answer := range []string{"q\n", "quit\n", "\n", ""} {
		var out bytes.Buffer
		chosen, handled := interactive(strings.NewReader(answer), &out)
		if !handled || chosen != nil {
			t.Fatalf("%q: handled=%v chose %v", answer, handled, chosen)
		}
		if !strings.Contains(out.String(), "Paired machines") {
			t.Fatalf("%q: it never showed the machines: %q", answer, out.String())
		}
	}
}

/*
An answer that is not one of them is said so, and asked again.

Not an exit: somebody who typed a letter by mistake has not asked to leave,
and a program that quit on a typo would be one nobody trusts with a list of
machines they can connect to.
*/
func TestANonsenseAnswerAsksAgain(t *testing.T) {
	aHomeWith(t, []devices.Device{aDevice("elythion", 1)})
	was := atATerminal
	t.Cleanup(func() { atATerminal = was })
	atATerminal = func() bool { return true }

	for _, answer := range []string{"x", "0", "2", "-1", "1.5", "elythion"} {
		var out bytes.Buffer
		chosen, handled := interactive(strings.NewReader(answer+"\nq\n"), &out)
		if !handled || chosen != nil {
			t.Fatalf("%q: handled=%v chose %v", answer, handled, chosen)
		}
		if !strings.Contains(out.String(), "is not one of them") {
			t.Fatalf("%q was taken for a machine: %q", answer, out.String())
		}
		// And it asked again rather than stopping: two prompts, not one.
		if strings.Count(out.String(), "to connect") < 2 {
			t.Fatalf("%q ended the question instead of repeating it: %q", answer, out.String())
		}
	}
}

/*
What the list looks like, which is the part somebody reads.

A name is peer-supplied and may be anything, including nothing — and a device
that arrived without one is still a device somebody can reach, so it is listed
under the only name it has.
*/
func TestTheListNamesEveryMachineAndItsKey(t *testing.T) {
	paired := []devices.Device{aDevice("elythion", 0xab), aDevice("", 0xcd)}
	shown := listing(paired)

	if !strings.Contains(shown, "1  elythion") {
		t.Fatalf("the first machine is not numbered 1: %q", shown)
	}
	if !strings.Contains(shown, "abababababababab") {
		t.Fatalf("a machine is listed without its key: %q", shown)
	}
	// The nameless one, under the only name it has.
	if !strings.Contains(shown, "2  cdcdcdcdcdcdcdcd") {
		t.Fatalf("a device with no name was listed with none: %q", shown)
	}
	// Numbering is what somebody types, so it starts at one and not at zero.
	if strings.Contains(shown, "    0  ") {
		t.Fatalf("the list is numbered from zero: %q", shown)
	}
}

/*
The numbers are stable, because somebody will learn them.

A person who connects to the same machine every day learns that it is `2`, and
a list that reshuffled between runs would send them to a different machine
with the same keystroke. The store is a *map* on disk — Go randomises map
order deliberately — so this depends on `Load` sorting, and that dependency is
worth pinning here rather than being an assumption two packages apart.
*/
func TestTheNumbersDoNotMoveBetweenRuns(t *testing.T) {
	home := aHomeWith(t, []devices.Device{
		aDevice("workshop", 3),
		aDevice("elythion", 1),
		aDevice("attic", 2),
	})

	var first string
	for range 8 {
		paired, err := devices.Load(devices.PathIn(home))
		if err != nil {
			t.Fatalf("load: %v", err)
		}
		shown := listing(paired)
		if first == "" {
			first = shown
			continue
		}
		if shown != first {
			t.Fatalf("the list reshuffled between reads:\n%s\nand\n%s", first, shown)
		}
	}
	// And it is in an order a person would expect to find something in.
	if strings.Index(first, "attic") > strings.Index(first, "elythion") {
		t.Fatalf("the machines are not in name order:\n%s", first)
	}
}

/*
What a chosen machine turns into, which is a command somebody could have typed.

The point of returning it rather than running it: an `exec` replaces this
process, so a test that let it happen would end at the first assertion. What
the answer *means* is the part worth pinning — that `2` is the second machine
in the list somebody is looking at, by name, and that `p` is `pair`.
*/
func TestAnAnswerBecomesACommand(t *testing.T) {
	aHomeWith(t, []devices.Device{aDevice("attic", 2), aDevice("elythion", 1)})
	was := atATerminal
	t.Cleanup(func() { atATerminal = was })
	atATerminal = func() bool { return true }

	for answer, want := range map[string][]string{
		// Sorted by name, so 1 is attic and 2 is elythion — the order the list
		// shows and therefore the order somebody counts in.
		"1": {"connect", "attic"},
		"2": {"connect", "elythion"},
		"p": {"pair"},
	} {
		var out bytes.Buffer
		chosen, handled := interactive(strings.NewReader(answer+"\n"), &out)
		if !handled {
			t.Fatalf("%q was not handled", answer)
		}
		if strings.Join(chosen, " ") != strings.Join(want, " ") {
			t.Fatalf("%q chose %v, want %v", answer, chosen, want)
		}
	}
}

/*
And the terminal test is the terminal's, not the file mode's.

`/dev/null` is a character device, so a mode check calls it a terminal — and
`esper < /dev/null`, which is a script, a service or a CI step, was offered a
list of machines and asked to pick one. Found by the smoke suite the first
time it ran this.
*/
func TestDevNullIsNotSomebodyToAsk(t *testing.T) {
	null, err := os.Open(os.DevNull)
	if err != nil {
		t.Skipf("no %s on this machine: %v", os.DevNull, err)
	}
	t.Cleanup(func() { _ = null.Close() })

	var termios unix.Termios
	if err := ioctlTermios(int(null.Fd()), &termios); err == nil {
		t.Fatalf("%s answered as a terminal", os.DevNull)
	}
	// And the mode check that used to decide says the opposite, which is why
	// this test exists rather than being obvious.
	info, err := null.Stat()
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if info.Mode()&os.ModeCharDevice == 0 {
		t.Skip("this machine's /dev/null is not a character device, so the trap is not here")
	}
}
