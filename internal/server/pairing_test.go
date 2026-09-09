package server

import (
	"strings"
	"testing"
	"time"
)

/*
The seam between two programs, and what it is made of.

Pairing belongs to the engine, so the control plane starts it and reads what it
says. It used to read what the engine *printed* — a regular expression over
prose written for the person holding a code — which made the engine's layout a
wire format nobody declared. `--json` is the declared one: one object per line,
as it happens, because a code that arrived only when the process ended would be
a code nobody could read out.

The text below is what the engine actually answers, captured from
`esper-engine pair --listen --json`, not written from memory.
*/
const engineOffered = `{"event":"offering","code":"74Y4-QNKK","address":"127.0.0.1:51110",` +
	`"port":51110,"expires_in":59,"fingerprint":{"hex":"3f2a1b0c9d8e7f60",` +
	`"words":["zinc","benign","rainbow","liberty","debate","signal","bunker","viking"]}}
`

func TestWhatTheEngineOffersIsWhatIsRead(t *testing.T) {
	before := time.Now()
	offer, err := readOffer(strings.NewReader(engineOffered))
	if err != nil {
		t.Fatalf("reading what the engine answered: %v", err)
	}

	if offer.Code != "74Y4-QNKK" {
		t.Errorf("code %q, and the engine offered 74Y4-QNKK", offer.Code)
	}
	if offer.Port != 51110 {
		t.Errorf("port %d, and the engine is listening on 51110", offer.Port)
	}
	if offer.State != "offering" {
		t.Errorf("state %q before anybody has paired", offer.State)
	}

	// Eight words, in the order they were given, because that is the order
	// they are said aloud and a pair of machines comparing them out of order
	// is two people disagreeing about a machine that is fine.
	want := []string{"zinc", "benign", "rainbow", "liberty", "debate", "signal", "bunker", "viking"}
	if len(offer.Words) != len(want) {
		t.Fatalf("read %d words, and a fingerprint is %d: %v", len(offer.Words), len(want), offer.Words)
	}
	for i := range want {
		if offer.Words[i] != want[i] {
			t.Errorf("word %d is %q and the engine said %q", i, offer.Words[i], want[i])
		}
	}

	// `expires_in` is a duration, and what a screen counts down is an instant
	// — so the reading is turned into one against this machine's clock as the
	// line is read.
	expires := time.Unix(offer.ExpiresAt, 0)
	if expires.Before(before.Add(58*time.Second)) || expires.After(time.Now().Add(60*time.Second)) {
		t.Errorf("expires at %v, and the engine said 59 seconds from about %v", expires, before)
	}
}

/*
Anything before the offer is not the offer.

A process writes to one stream, and a library, a warning or a note about the
device list can land on it before the engine has anything to say. A line that
is not an offer is skipped — and one that is not JSON at all is skipped too,
rather than stopping a pairing that is about to work.
*/
func TestALineThatIsNotTheOfferIsSkipped(t *testing.T) {
	offer, err := readOffer(strings.NewReader(
		"  · the device list needed correcting: one record was dropped\n" +
			`{"event":"joining","address":"127.0.0.1:1"}` + "\n" +
			"\n" +
			engineOffered,
	))
	if err != nil {
		t.Fatalf("reading: %v", err)
	}
	if offer.Code != "74Y4-QNKK" || len(offer.Words) != 8 {
		t.Errorf("the offer did not survive the lines before it: %+v", offer)
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
	said := "esper-engine pair: either `pair --listen <addr>` to offer a code, " +
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
		t.Error("an engine that answered with nothing produced an offer")
	}
}

/*
Half an offer is not an offer.

One line carries the whole of it, so there is no reason to accept part: a code
with no words is a screen asking somebody to read aloud a fingerprint it does
not have, and a code with no port is one nothing can connect to. Both would
look like a working pairing screen right up until the moment they matter.
*/
func TestHalfAnOfferIsRefused(t *testing.T) {
	for _, said := range []string{
		`{"event":"offering","port":51110,"fingerprint":{"words":["zinc"]}}`,
		`{"event":"offering","code":"74Y4-QNKK","fingerprint":{"words":["zinc"]}}`,
		`{"event":"offering","code":"74Y4-QNKK","port":51110}`,
		`{"event":"offering","code":"74Y4-QNKK","port":51110,"fingerprint":{"words":[]}}`,
	} {
		offer, err := readOffer(strings.NewReader(said + "\n"))
		if err == nil {
			t.Errorf("%s was read as a usable offer: %+v", said, offer)
		}
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
