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
	if sensitive != 5 {
		t.Fatalf("five permissions need a person at the machine, this counts %d", sensitive)
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
