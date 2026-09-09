package devices

import (
	"context"
	"errors"
	"strings"
	"testing"
)

const anFP = "ea834d4345a142c7"

func TestAGrantBecomesTheEnginesOwnCommand(t *testing.T) {
	got, err := command(Grant, anFP, []Permission{View, Control})
	if err != nil {
		t.Fatalf("a plain grant: %v", err)
	}
	want := []string{"grant", anFP, "view", "control"}
	if len(got) != len(want) {
		t.Fatalf("got %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("got %v, want %v", got, want)
		}
	}
}

func TestTheOrderThePermissionsCameInIsKept(t *testing.T) {
	// The engine reports what it changed, and a report in a different order
	// from the request reads as a different change.
	got, err := command(Revoke, anFP, []Permission{Terminal, Audio, View})
	if err != nil {
		t.Fatalf("%v", err)
	}
	if strings.Join(got, " ") != "revoke "+anFP+" terminal audio view" {
		t.Fatalf("got %v", got)
	}
}

// TestASensitivePermissionIsNotGrantedFromHere is the rule the whole file
// exists for. `unattended` is the permission that *removes* the question a
// session is held up by, and the host prompt is why it cannot be switched on
// silently — so a request from the network is refused rather than answered
// with `--yes` on somebody's behalf.
func TestASensitivePermissionIsNotGrantedFromHere(t *testing.T) {
	sensitive := 0
	for _, p := range AllPermissions {
		if !p.Sensitive() {
			continue
		}
		sensitive++
		_, err := command(Grant, anFP, []Permission{p})
		if !errors.Is(err, ErrHostOnly) {
			t.Errorf("granting %q should be refused here, got %v", p, err)
			continue
		}
		if !strings.Contains(err.Error(), "at the machine") {
			t.Errorf("the refusal for %q should say where to do it: %v", p, err)
		}
		if !strings.Contains(err.Error(), string(p)) {
			t.Errorf("the refusal for %q should name it: %v", p, err)
		}
	}
	if sensitive != 6 {
		t.Fatalf("six permissions need a person at the machine, this counts %d", sensitive)
	}
	// **And `terminal` is one of them**, which it was not. A shell runs as the
	// user who owns this machine's Esper state: it can read the identity key
	// and this API's own token, rewrite the device list without going near
	// `grant`, and edit the record. So it grants every other permission on
	// this list whether or not anybody grants them — and it was the one the
	// window could switch on by itself.
	if !Terminal.Sensitive() {
		t.Error("granting a shell from this API needs nobody at the machine")
	}
}

// TestASensitivePermissionIsStillRevocableFromHere is the other half, and it
// is not symmetry for its own sake: a revocation that could be refused would
// be one somebody could be talked out of.
func TestASensitivePermissionIsStillRevocableFromHere(t *testing.T) {
	for _, p := range AllPermissions {
		if !p.Sensitive() {
			continue
		}
		if _, err := command(Revoke, anFP, []Permission{p}); err != nil {
			t.Errorf("revoking %q must not be refused: %v", p, err)
		}
	}
}

// TestOneSensitivePermissionRefusesTheWholeRequest: a partial grant would be
// worse than none, because the caller would have to work out which half
// happened.
func TestOneSensitivePermissionRefusesTheWholeRequest(t *testing.T) {
	_, err := command(Grant, anFP, []Permission{View, Unattended, Control})
	if !errors.Is(err, ErrHostOnly) {
		t.Fatalf("expected the whole request refused, got %v", err)
	}
}

func TestEveryOrdinaryPermissionCanBeGrantedFromHere(t *testing.T) {
	for _, p := range AllPermissions {
		if p.Sensitive() {
			continue
		}
		if _, err := command(Grant, anFP, []Permission{p}); err != nil {
			t.Errorf("granting %q should be allowed here: %v", p, err)
		}
	}
}

// TestAPrefixIsRefusedEvenThoughTheEngineWouldTakeIt is the interesting one.
// The engine accepts a fingerprint prefix and refuses an ambiguous one, which
// is right for somebody typing. A caller that read the fingerprint out of
// `GET /devices` has all sixteen characters — and a prefix that is unambiguous
// today can match a second device tomorrow, which would silently change the
// wrong one.
func TestAPrefixIsRefusedEvenThoughTheEngineWouldTakeIt(t *testing.T) {
	for _, bad := range []string{
		"ea834d43",           // a prefix the engine would accept today
		"",                   // nothing
		anFP + "0",           // too long
		"EA834D4345A142C7",   // uppercase: the list gives lowercase
		"ea834d4345a142cg",   // not hex
		"ea834d43-45a1-42c7", // punctuated
		"ea834d4345a142c 7",  // spaced
	} {
		_, err := command(Grant, bad, []Permission{View})
		if !errors.Is(err, ErrNoSuchDevice) {
			t.Errorf("%q should be refused as a fingerprint, got %v", bad, err)
		}
	}
}

func TestAnUnknownPermissionIsRefused(t *testing.T) {
	if _, err := command(Grant, anFP, []Permission{"root"}); err == nil {
		t.Fatal("a word nobody granted must be refused")
	}
}

// TestAPermissionNamedTwiceIsRefused: granting it once quietly is how a
// caller's bug survives.
func TestAPermissionNamedTwiceIsRefused(t *testing.T) {
	_, err := command(Grant, anFP, []Permission{View, Control, View})
	if err == nil || !strings.Contains(err.Error(), "twice") {
		t.Fatalf("expected a refusal naming the repeat, got %v", err)
	}
}

func TestAChangeWithNoPermissionsIsRefused(t *testing.T) {
	for _, what := range []Change{Grant, Revoke} {
		if _, err := command(what, anFP, nil); err == nil {
			t.Errorf("%s with no permissions should be refused", what.verb())
		}
	}
}

func TestForgettingTakesNoPermissions(t *testing.T) {
	got, err := command(Forget, anFP, nil)
	if err != nil {
		t.Fatalf("forgetting: %v", err)
	}
	if strings.Join(got, " ") != "forget "+anFP {
		t.Fatalf("got %v", got)
	}
	// Passing permissions to `forget` is a caller that means something else.
	if _, err := command(Forget, anFP, []Permission{View}); err == nil {
		t.Fatal("forget does not take permissions")
	}
}

// TestTheFingerprintIsAlwaysAnArgumentAndNeverAWord: the argv is built as a
// slice and handed to the process directly, so there is no shell to quote for
// — but the shape is asserted so that nobody later joins it into a string.
func TestTheFingerprintIsAlwaysAnArgumentAndNeverAWord(t *testing.T) {
	args, err := command(Forget, anFP, nil)
	if err != nil {
		t.Fatalf("%v", err)
	}
	if len(args) != 2 {
		t.Fatalf("expected a verb and a fingerprint, got %v", args)
	}
	if args[1] != anFP {
		t.Fatalf("the fingerprint should be its own argument, got %v", args)
	}
}

func TestAWriterWithNoEngineSaysSo(t *testing.T) {
	// The device store is written by one program, and if it is not there the
	// answer is not "wrote it myself".
	w := Writer{}
	err := w.Change(context.Background(), Revoke, anFP, []Permission{View})
	if err == nil || !strings.Contains(err.Error(), "written by one") {
		t.Fatalf("expected a refusal naming why, got %v", err)
	}
}

// **A home of nothing is refused before a process is spawned.** `ESPER_HOME=`
// makes every path the engine resolves relative, so a write would land in
// whatever directory this process started in while the read afterwards came
// from the real store — a change that reports success and did nothing where
// anybody would look.
func TestAHomeOfNothingIsRefused(t *testing.T) {
	for _, home := range []string{"", "   "} {
		w := Writer{Engine: "/bin/true", Home: home}
		err := w.Change(context.Background(), Grant, "0123456789abcdef", []Permission{View})
		if err == nil {
			t.Fatalf("a home of %q was accepted", home)
		}
		if !strings.Contains(err.Error(), "has to be somewhere") {
			t.Errorf("a home of %q was refused for the wrong reason: %v", home, err)
		}
	}
	// And the same request with a home is not refused, or the test above
	// passes for any reason at all.
	w := Writer{Engine: "/bin/true", Home: t.TempDir()}
	if err := w.Change(context.Background(), Grant, "0123456789abcdef", []Permission{View}); err != nil {
		t.Fatalf("a request with a real home was refused: %v", err)
	}
}

// TestKeepNamesADeviceAndNothingElse — `keep` changes how long a pairing
// lasts, not what the device may do, so permissions are a caller's mistake
// rather than something to ignore.
func TestKeepNamesADeviceAndNothingElse(t *testing.T) {
	t.Parallel()
	const fp = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"

	args, err := command(Keep, fp[:FingerprintHex], nil)
	if err != nil {
		t.Fatalf("keep: %v", err)
	}
	if len(args) != 2 || args[0] != "keep" || args[1] != fp[:FingerprintHex] {
		t.Errorf("keep should be `keep <fingerprint>`, got %v", args)
	}

	if _, err := command(Keep, fp[:FingerprintHex], []Permission{View}); err == nil {
		t.Error("keep with permissions should be refused, not quietly obeyed")
	}

	// And the fingerprint check applies to it like everything else: whatever
	// reaches this becomes an argument to the engine.
	if _, err := command(Keep, "nonsense", nil); !errors.Is(err, ErrNoSuchDevice) {
		t.Errorf("keep should refuse a fingerprint that is not one, got %v", err)
	}
}

/*
What a schedule may put on a command line.

The `when` comes from a caller and reaches the engine's argv, so the shape it
has to have is decided here — an allowlist of what a schedule is made of, not
an attempt to remove what is dangerous. The engine's parser is what decides
whether those characters spell a schedule it *knows*; this decides what is
worth handing it at all.
*/
func TestOnlyAScheduleShapedWhenReachesTheEngine(t *testing.T) {
	const fp = "827aabec450ddf13"

	for _, good := range []string{
		"always",
		"clear",
		"weekdays 08:00-18:00",
		"weekends 10:00-23:00",
		"daily 22:00-06:00",
		"mon,wed,fri 9-17",
	} {
		args, err := command(Schedule, fp, []Permission{Permission(good)})
		if err != nil {
			t.Fatalf("%q was refused: %v", good, err)
		}
		if len(args) != 3 || args[0] != "schedule" || args[1] != fp || args[2] != good {
			t.Fatalf("%q became %v", good, args)
		}
	}

	for _, bad := range []struct {
		what string
		when string
	}{
		{"empty", ""},
		{"only spaces", "   "},
		{"a flag", "--yes"},
		{"shell metacharacters", "weekdays 08:00-18:00; rm -rf /"},
		{"a pipe", "always|sh"},
		{"a newline and a second argument", "always\nforget 827aabec450ddf13"},
		{"a nul", "always\x00"},
		{"backticks", "`id`"},
		{"a dollar", "$(id)"},
		{"uppercase, which the engine does not write", "WEEKDAYS 08:00-18:00"},
		{"longer than any schedule", "mon," + "tue," + "wed," + "thu," + "fri," + "sat," + "sun," + "mon,tue,wed 08:00-18:00"},
	} {
		if _, err := command(Schedule, fp, []Permission{Permission(bad.when)}); err == nil {
			t.Fatalf("%s (%q) was accepted onto a command line", bad.what, bad.when)
		}
	}

	// And a schedule names exactly one when: two would be two arguments the
	// engine reads as a day list and something else.
	if _, err := command(Schedule, fp, nil); err == nil {
		t.Fatal("a schedule with no when was accepted")
	}
	if _, err := command(Schedule, fp, []Permission{"always", "clear"}); err == nil {
		t.Fatal("a schedule with two whens was accepted")
	}
	// The fingerprint rule is the same one every other change uses.
	if _, err := command(Schedule, "827aabec", []Permission{"always"}); err == nil {
		t.Fatal("a fingerprint prefix was accepted")
	}
}

/*
Four words reach the engine, and nothing else does.

A lock-after is a closed set rather than a grammar, so unlike a schedule it is
checked here: what a caller sends is one of the words this build knows or it
is refused by name. That makes the set the thing under test — a build that
grew a fourth choice in Rust and not here would refuse a word the engine
understands, and a build that let anything through would be putting a caller's
string on a command line.
*/
func TestOnlyAKnownLockAfterWordReachesTheEngine(t *testing.T) {
	const fp = "827aabec450ddf13"

	// Every choice this build knows, plus the one that takes the answer off.
	want := make([]string, 0, len(AllLockAfter)+1)
	for _, a := range AllLockAfter {
		want = append(want, string(a))
	}
	want = append(want, LockDefault)
	for _, good := range want {
		args, err := command(LockAfterChange, fp, []Permission{Permission(good)})
		if err != nil {
			t.Fatalf("%q was refused: %v", good, err)
		}
		if len(args) != 3 || args[0] != "lock-after" || args[1] != fp || args[2] != good {
			t.Fatalf("%q became %v", good, args)
		}
	}

	for _, bad := range []struct {
		what string
		word string
	}{
		{"empty", ""},
		{"a flag", "--yes"},
		{"the engine's other spelling, which this side does not take", "host"},
		{"a word nobody knows", "sometimes"},
		{"shell metacharacters", "always; rm -rf /"},
		{"a newline and a second argument", "always\nforget 827aabec450ddf13"},
		{"a nul", "always\x00"},
		{"uppercase, which the store does not hold", "ALWAYS"},
	} {
		if _, err := command(LockAfterChange, fp, []Permission{Permission(bad.word)}); err == nil {
			t.Fatalf("%s (%q) was accepted onto a command line", bad.what, bad.word)
		}
	}

	// One word, never none and never two.
	if _, err := command(LockAfterChange, fp, nil); err == nil {
		t.Fatal("a lock-after with no word was accepted")
	}
	if _, err := command(LockAfterChange, fp, []Permission{"always", "never"}); err == nil {
		t.Fatal("a lock-after with two words was accepted")
	}
	// The fingerprint rule is the same one every other change uses.
	if _, err := command(LockAfterChange, "827aabec", []Permission{"always"}); err == nil {
		t.Fatal("a fingerprint prefix was accepted")
	}
}

// And the refusal says what the words are, because a caller that guessed wrong
// has no other way to find out.
func TestARefusedLockAfterNamesTheChoices(t *testing.T) {
	_, err := command(LockAfterChange, "827aabec450ddf13", []Permission{"sometimes"})
	if err == nil {
		t.Fatal("an unknown word was accepted")
	}
	for _, a := range AllLockAfter {
		if !strings.Contains(err.Error(), string(a)) {
			t.Fatalf("the refusal does not name %q: %v", a, err)
		}
	}
	if !strings.Contains(err.Error(), LockDefault) {
		t.Fatalf("the refusal does not say how to take the answer off: %v", err)
	}
}

/*
A wake names a device and nothing else, and never names a destination.

The engine takes `--to <address>` for a segment this machine is not on. The
control plane does not pass it, and this is what says so: a caller that could
name where a packet goes could use this machine to send packets somewhere of
its choosing, and the only difference between that and a wake is one argument.
*/
func TestAWakeNamesADeviceAndNoDestination(t *testing.T) {
	const fp = "827aabec450ddf13"

	args, err := command(Wake, fp, nil)
	if err != nil {
		t.Fatalf("a wake was refused: %v", err)
	}
	if len(args) != 2 || args[0] != "wake" || args[1] != fp {
		t.Fatalf("a wake became %v, and it is the verb and the device", args)
	}
	for _, arg := range args {
		if strings.HasPrefix(arg, "-") {
			t.Fatalf("a wake carries the flag %q", arg)
		}
	}

	// Nothing may be smuggled alongside it. `command` takes permissions, and
	// a wake that quietly accepted one would be putting a caller's string
	// after the fingerprint — which for `wake` is where `--to` goes.
	for _, extra := range []Permission{"view", "--to", "--to 10.0.0.1", ""} {
		if _, err := command(Wake, fp, []Permission{extra}); err == nil {
			t.Fatalf("a wake carrying %q was accepted", extra)
		}
	}

	// The fingerprint rule is the same one every other change uses.
	if _, err := command(Wake, "827aabec", nil); err == nil {
		t.Fatal("a fingerprint prefix was accepted")
	}
	if _, err := command(Wake, "827aabec450ddf1Z", nil); err == nil {
		t.Fatal("a fingerprint that is not hex was accepted")
	}
}

/*
The three that reach another machine name a device and nothing else.

Like a wake, and for the same reason: the argv is where a caller's string
would become a flag if anything unchecked reached it. What is different is
that these can *do* something at the other end, so the shape of what runs is
worth pinning rather than assuming.
*/
func TestTheReachingActionsNameADeviceAndNothingElse(t *testing.T) {
	const fp = "827aabec450ddf13"

	for what, verb := range map[Change]string{
		LockScreen: "lock",
		LogOff:     "logoff",
		Restart:    "reboot",
	} {
		args, err := command(what, fp, nil)
		if err != nil {
			t.Fatalf("%s was refused: %v", verb, err)
		}
		if len(args) != 2 || args[0] != verb || args[1] != fp {
			t.Fatalf("%s became %v, and it is the verb and the device", verb, args)
		}
		for _, arg := range args {
			if strings.HasPrefix(arg, "-") {
				t.Fatalf("%s carries the flag %q", verb, arg)
			}
		}
		// Nothing may be smuggled alongside: `--via` and `--broker-key` are
		// where a caller could name where this machine connects to.
		for _, extra := range []Permission{"view", "--via", "--via 10.0.0.1:1", ""} {
			if _, err := command(what, fp, []Permission{extra}); err == nil {
				t.Fatalf("%s carrying %q was accepted", verb, extra)
			}
		}
		if _, err := command(what, "827aabec", nil); err == nil {
			t.Fatalf("%s took a fingerprint prefix", verb)
		}
		if !what.Reaching() {
			t.Fatalf("%s is not counted as an action on another machine, so it gets a file write's patience", verb)
		}
	}

	// And the ones that only touch this machine's own store are not.
	for _, local := range []Change{Grant, Revoke, Forget, Keep, Schedule, LockAfterChange} {
		if local.Reaching() {
			t.Fatalf("%s is counted as reaching another machine, which it does not", local.verb())
		}
	}
}

/*
A group names the group first, and only a name reaches that position.

`group <name> <fingerprint>...` reads its first argument as the group — so a
name beginning with `-` would be a flag there, and one with a space would be
two arguments, the second of which the engine reads as a fingerprint. That is
a different trap from the other changes here, where the fingerprint comes
first and is checked to be hex.
*/
func TestOnlyAGroupShapedNameReachesTheEngine(t *testing.T) {
	const fp = "827aabec450ddf13"

	// Any script, because the device list already holds a group named in one:
	// a name the window can show and never change is not a name it supports.
	for _, good := range []string{"ops", "the-lab", "site_2", "A1", "東京", "mköbenhavn"} {
		args, err := command(Group, fp, []Permission{Permission(good)})
		if err != nil {
			t.Fatalf("%q was refused: %v", good, err)
		}
		if len(args) != 3 || args[0] != "group" || args[1] != good || args[2] != fp {
			t.Fatalf("%q became %v, and it is the verb, the group and the device", good, args)
		}
		out, err := command(Ungroup, fp, []Permission{Permission(good)})
		if err != nil {
			t.Fatalf("removing from %q was refused: %v", good, err)
		}
		if len(out) != 4 || out[3] != "--remove" {
			t.Fatalf("removing from %q became %v", good, out)
		}
	}

	for _, bad := range []struct {
		what string
		name string
	}{
		{"empty", ""},
		{"only spaces", "   "},
		{"a flag", "--remove"},
		{"a flag in the group's place", "-x"},
		{"two arguments", "ops elythion"},
		{"shell metacharacters", "ops; rm -rf /"},
		{"a pipe", "ops|sh"},
		{"a newline and a second argument", "ops\nforget 827aabec450ddf13"},
		{"a nul", "ops\x00"},
		{"backticks", "`id`"},
		{"a dollar", "$(id)"},
		{"a path", "../ops"},
		{"longer than a name", strings.Repeat("o", MostGroupChars+1)},
		// Bytes are not characters: a name of this many letters in a
		// three-byte script is the same length as one in a one-byte script.
		{"longer than a name in another script", strings.Repeat("東", MostGroupChars+1)},
		{"a right-to-left override, which makes a name display as another", "op\u202eslo"},
		{"a zero-width space", "op\u200bs"},
		{"a non-breaking space", "op\u00a0s"},
	} {
		if _, err := command(Group, fp, []Permission{Permission(bad.name)}); err == nil {
			t.Fatalf("%s (%q) was accepted onto a command line", bad.what, bad.name)
		}
		if _, err := command(Ungroup, fp, []Permission{Permission(bad.name)}); err == nil {
			t.Fatalf("%s (%q) was accepted when removing", bad.what, bad.name)
		}
	}

	// Exactly one group, and a whole fingerprint, like everything else here.
	if _, err := command(Group, fp, nil); err == nil {
		t.Fatal("a group change with no group was accepted")
	}
	if _, err := command(Group, fp, []Permission{"ops", "lab"}); err == nil {
		t.Fatal("a group change naming two groups was accepted")
	}
	if _, err := command(Group, "827aabec", []Permission{"ops"}); err == nil {
		t.Fatal("a fingerprint prefix was accepted")
	}
	// And a group is not an action on another machine: it is a file write.
	if Group.Reaching() || Ungroup.Reaching() {
		t.Fatal("a group change is counted as reaching another machine")
	}
}
