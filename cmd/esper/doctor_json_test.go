package main

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/elythi0n/esper/internal/proto"
)

// The printed ladder and the one a script reads are the same ladder.
//
// Two reports of one fact drift the moment somebody adds a rung to whichever
// they were looking at. This is the check that notices.
func TestBothLaddersNameTheSameRungs(t *testing.T) {
	status := &proto.Status{MDNS: true, Overlays: []string{"wg0"}}

	printed := printed(t, func() { reachTiers(status) })
	rungs := waysIn(status)
	if len(rungs) == 0 {
		t.Fatal("the ladder has no rungs at all, so this check measures nothing")
	}
	for _, r := range rungs {
		if !strings.Contains(printed, r.Name) {
			t.Errorf("the rung %q is in the object and not in the report:\n%s", r.Name, printed)
		}
		// The first few words of the detail, which is enough to catch a rung
		// whose two reports say different things about the same machine.
		words := strings.Fields(strings.TrimPrefix(r.Detail, "unavailable — "))
		if len(words) >= 3 {
			opening := strings.Join(words[:3], " ")
			if !strings.Contains(printed, opening) {
				t.Errorf("the rung %q reads %q to a script and something else to a person:\n%s", r.Name, opening, printed)
			}
		}
	}
	// And nothing in the report that is not in the object. The report's own
	// heading and the warning paragraph are prose, so this counts rungs by
	// their marks.
	marks := strings.Count(printed, "  ✓ ") + strings.Count(printed, "  · ")
	if marks != len(rungs) {
		t.Errorf("the report has %d rung(s) and the object %d:\n%s", marks, len(rungs), printed)
	}
}

// `remembered` is neither available nor unavailable: it belongs to the
// controller's store, and a host cannot see whether somebody else remembers
// it. A script has to be able to tell that apart from a no.
func TestARungThisMachineCannotKnowIsNotAnAnswer(t *testing.T) {
	rungs := waysIn(&proto.Status{})
	var found *rung
	for i := range rungs {
		if rungs[i].Name == "remembered" {
			found = &rungs[i]
		}
	}
	if found == nil {
		t.Fatal("the ladder has no `remembered` rung")
	}
	if found.Available != nil {
		t.Errorf("this machine claimed to know whether somebody remembers it: %v", *found.Available)
	}
	// And everything else does answer, either way.
	for _, r := range rungs {
		if r.Name != "remembered" && r.Available == nil {
			t.Errorf("the rung %q gave no answer at all", r.Name)
		}
	}
}

// Readiness is the screen and the encoder. Input is reported and not counted:
// a machine that can be watched and not driven can still host.
func TestInputIsNotCountedAgainstReadiness(t *testing.T) {
	can := proto.Capability{Available: true, Detail: "yes"}
	cannot := proto.Capability{Available: false, Detail: "no"}

	if !hostReady(&proto.Status{Capture: can, Encoder: can, Input: cannot}) {
		t.Error("a machine that can be watched and not driven was called unable to host")
	}
	if hostReady(&proto.Status{Capture: cannot, Encoder: can, Input: can}) {
		t.Error("a machine with no screen was called ready")
	}
	if hostReady(&proto.Status{Capture: can, Encoder: cannot, Input: can}) {
		t.Error("a machine that cannot encode was called ready")
	}
}

// The self test says the same things to a person and to a script.
func TestTheSelfTestSaysTheSameThingBothWays(t *testing.T) {
	walk := func(say sayer) {
		say.step("paired", true, "two identities, in 300 ms")
		say.figures(map[string]float64{"seconds": 0.3})
		say.step("session", false, "the session did not open: nothing answered")
		say.evidence([]string{"host: refused", "", "  reason 402"})
		say.warn("the two ends found each other and agreed a session, so the\nnetwork and the pairing are fine.")
	}

	kept := &collecting{}
	walk(kept)
	shown := printed(t, func() { walk(printing{}) })

	if len(kept.Steps) != 2 {
		t.Fatalf("two steps were reported and %d kept: %+v", len(kept.Steps), kept.Steps)
	}
	for _, s := range kept.Steps {
		if !strings.Contains(shown, s.Detail) {
			t.Errorf("the kept step %q is not in what a person saw:\n%s", s.Detail, shown)
		}
	}
	if kept.Steps[0].OK == kept.Steps[1].OK {
		t.Error("a failed step and a passed one were kept the same way")
	}
	// Evidence belongs to the step it explains, and the blank line a terminal
	// needs is not evidence.
	if got := kept.Steps[1].Evidence; len(got) != 2 || got[0] != "host: refused" {
		t.Errorf("the evidence for the failure is %q", got)
	}
	if kept.Steps[0].Evidence != nil {
		t.Errorf("evidence landed on the step before the one it explains: %q", kept.Steps[0].Evidence)
	}
	if got := kept.Steps[0].Figures["seconds"]; got != 0.3 {
		t.Errorf("the figures behind the first step are %v", kept.Steps[0].Figures)
	}
	if len(kept.Warnings) != 1 || strings.Contains(kept.Warnings[0], "\n") {
		t.Errorf("a warning written to be read down a column was kept as %q", kept.Warnings)
	}
}

// A kept report has to survive being written out, which is the only thing it
// is for.
func TestAKeptSelfTestEncodes(t *testing.T) {
	kept := &collecting{Steps: []selfStep{}, Warnings: []string{}}
	kept.step("paired", true, "two identities")
	kept.verdict(true, "this machine can hold a session with itself")

	raw, err := json.Marshal(kept)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	var back collecting
	if err := json.Unmarshal(raw, &back); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !back.OK || len(back.Steps) != 1 || back.Steps[0].Name != "paired" {
		t.Errorf("a kept report came back as %+v", back)
	}
}
