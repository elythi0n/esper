package server

import (
	"strings"
	"testing"
	"time"
)

/*
The seam between two programs, which is a regular expression.

Pairing belongs to the engine, so the control plane starts it and *reads what
it prints*. That makes the engine's output a wire format nobody declared: the
day somebody reindents a line or rewords "expires in", this control plane
stops finding a code and the screen shows a spinner for ever with nothing in
any log to say why.

So the text below is what the engine actually prints — captured from
`esper-engine pair --listen`, not written from memory — and these tests are the
thing that fails when it changes.
*/
const engineOffered = `🔗 pairing

  code       74Y4-QNKK
  listening  127.0.0.1:51110
  expires    in 59 seconds

  On the other machine:
      esper-engine pair <this-machine>:51110 --code 74Y4-QNKK

  This device's fingerprint — read it aloud and check it matches:
      zinc benign rainbow liberty
      debate signal bunker viking
`

func TestWhatTheEngineOffersIsWhatIsRead(t *testing.T) {
	before := time.Now()
	offer, err := readOffer(strings.NewReader(engineOffered))
	if err != nil {
		t.Fatalf("reading what the engine printed: %v", err)
	}

	if offer.Code != "74Y4-QNKK" {
		t.Errorf("code %q, and the engine printed 74Y4-QNKK", offer.Code)
	}
	if offer.Port != 51110 {
		t.Errorf("port %d, and the engine is listening on 51110", offer.Port)
	}
	if offer.State != "offering" {
		t.Errorf("state %q before anybody has paired", offer.State)
	}

	// Eight words, in the order they were read, because that is the order they
	// are said aloud and a pair of machines comparing them out of order is two
	// people disagreeing about a machine that is fine.
	want := []string{"zinc", "benign", "rainbow", "liberty", "debate", "signal", "bunker", "viking"}
	if len(offer.Words) != len(want) {
		t.Fatalf("read %d words, and a fingerprint is %d: %v", len(offer.Words), len(want), offer.Words)
	}
	for i := range want {
		if offer.Words[i] != want[i] {
			t.Errorf("word %d is %q and the engine said %q", i, offer.Words[i], want[i])
		}
	}

	// "expires in 59 seconds" is a duration, and what a screen counts down is
	// an instant — so the reading is turned into one against this machine's
	// clock as the line is read.
	expires := time.Unix(offer.ExpiresAt, 0)
	if expires.Before(before.Add(58*time.Second)) || expires.After(time.Now().Add(60*time.Second)) {
		t.Errorf("expires at %v, and the engine said 59 seconds from about %v", expires, before)
	}
}

/*
A code is only read once there is one to read.

The offer line and the fingerprint are both indented, and the fingerprint's
lines are just words — so anything word-shaped that arrives *before* the code
would be collected as part of it. The parser will not start on the words until
it has a code, and this is the line that proves it: a machine name that looks
exactly like a fingerprint line.
*/
func TestWordsBeforeACodeAreNotAFingerprint(t *testing.T) {
	offer, err := readOffer(strings.NewReader(
		"      some machine said something first\n" + engineOffered,
	))
	if err != nil {
		t.Fatalf("reading: %v", err)
	}
	for _, w := range offer.Words {
		if w == "some" || w == "machine" {
			t.Fatalf("a line before the code was read as part of the fingerprint: %v", offer.Words)
		}
	}
	if len(offer.Words) != 8 {
		t.Errorf("read %d words after an early line: %v", len(offer.Words), offer.Words)
	}
}

/*
An engine that offered nothing says why, and the reason is the engine's own.

This is what somebody sees when they ask to pair and the engine refuses — a
missing key, a port already taken, the wrong arguments — so the sentence it
printed has to survive into the error rather than being replaced by "pairing
failed".
*/
func TestAnEngineThatOffersNoCodeSaysWhy(t *testing.T) {
	said := "pair: either `pair --listen <addr>` to offer a code, " +
		"or `pair <addr> --code <code>` to use one"
	offer, err := readOffer(strings.NewReader(said + "\n"))
	if err == nil {
		t.Fatalf("no code was offered and an offer came back: %+v", offer)
	}
	if !strings.Contains(err.Error(), "pair <addr> --code") {
		t.Errorf("the engine said %q and the error is %q", said, err)
	}

	// And nothing at all is still an error, not an empty offer somebody polls
	// for ever.
	if _, err := readOffer(strings.NewReader("")); err == nil {
		t.Error("an engine that printed nothing produced an offer")
	}
}

/*
Half an offer is still an offer.

The engine prints the code before the fingerprint, and the reader returns as
soon as it has both — but if the engine is killed between the two, what it did
say is worth keeping: a code with no words is a code somebody can still use,
and refusing it would turn a partial read into no pairing at all.
*/
func TestACodeWithoutItsWordsIsStillUsable(t *testing.T) {
	cut := strings.Index(engineOffered, "  This device's fingerprint")
	if cut < 0 {
		t.Fatal("the captured output no longer has a fingerprint section")
	}
	offer, err := readOffer(strings.NewReader(engineOffered[:cut]))
	if err != nil {
		t.Fatalf("reading an offer that stopped early: %v", err)
	}
	if offer.Code != "74Y4-QNKK" || offer.Port != 51110 {
		t.Errorf("the code and port did not survive a short read: %+v", offer)
	}
	if len(offer.Words) != 0 {
		t.Errorf("words appeared where the engine printed none: %v", offer.Words)
	}
}

/*
What the screen polls, and why it is a copy.

`snapshot` is read by an HTTP handler while the pairing goroutine is still
writing to the same offer. Handing back the pointer would mean a response
being serialised as the state changes under it — and a caller that kept it
could change what this machine believes it is offering.
*/
func TestASnapshotIsACopyAndNotTheOffer(t *testing.T) {
	var p pairing
	if p.snapshot() != nil {
		t.Fatal("a pairing that has offered nothing produced an offer")
	}

	p.set(&Offer{Code: "74Y4-QNKK", Port: 51110, State: "offering"})
	seen := p.snapshot()
	if seen == nil {
		t.Fatal("nothing came back from a pairing that is offering")
	}

	seen.Code = "0000-0000"
	seen.State = "paired"
	if again := p.snapshot(); again.Code != "74Y4-QNKK" || again.State != "offering" {
		t.Errorf("changing a snapshot changed the offer: %+v", again)
	}

	p.finish("failed", "the other machine hung up")
	after := p.snapshot()
	if after.State != "failed" || after.Detail != "the other machine hung up" {
		t.Errorf("finish did not reach the offer: %+v", after)
	}
	// Finishing says how it ended; it does not throw away what it was.
	if after.Code != "74Y4-QNKK" {
		t.Errorf("the code was lost when the offer finished: %+v", after)
	}
}

/*
Ending something that never started.

`stop` is called from the handler for "cancel", from the shutdown path, and
again by the goroutine as it unwinds — so it has to be safe with no offer
running and safe twice. A panic here is the control plane dying because
somebody pressed cancel on a screen that had already moved on.
*/
func TestStoppingWhatWasNeverStartedIsSafe(t *testing.T) {
	var p pairing
	p.stop()
	p.stop()

	stopped := 0
	p.cancel = func() { stopped++ }
	p.stop()
	p.stop()
	if stopped != 1 {
		t.Errorf("the offer was cancelled %d times, and it runs once", stopped)
	}

	// And finishing a pairing that has no offer is a no-op rather than a nil
	// dereference: the goroutine reports how it ended whether or not anybody
	// is still holding an offer to record it on.
	p.finish("expired", "")
	if p.snapshot() != nil {
		t.Error("finishing without an offer invented one")
	}
}
