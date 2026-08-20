package server

import (
	"strings"
	"testing"
	"time"
)

/*
The other half of the seam, and the same undeclared format.

An invitation belongs to the engine too, so the control plane runs it and reads
what it prints. Both texts below were captured from real runs — the first from
`esper-engine invite --for tester --via …` against a broker started for the
purpose, the second from the same command with no broker configured — because a
fixture written from memory tests the memory.
*/
const engineInvited = `✉️  esper invite

  for        tester
  broker     127.0.0.1:7443
  expires    in 30 minute(s)

  Send them this link, and nothing else:

      esper://join/AZu9yNmY-3a4GuP2Idvm1QS5P_1lj8DRn7br382eE6E9q18cWCU8qxLqLfBjoi799HERXh3i-a_rCkDG6CuXAEgAAAAAaqQ-OQ4xMjcuMC4wLjE6NzQ0MwlSM1FILVBHUlI

  They run:  esper-engine join <the link>

  This device's fingerprint — read it aloud and check it matches:
      jockey agree matrix splash
      vibrant maple slate squad
`

const engineHasNoBroker = `esper-engine invite: invite needs a broker you run: --via <address> --broker-key <key>, or broker.address and broker.key in your settings.
  There is no default broker anywhere in Esper, and this will not use somebody else's.
  Start one with: esper-broker serve --listen :7443
`

func TestWhatTheEngineInvitesIsWhatIsRead(t *testing.T) {
	before := time.Now()
	invitation, err := readInvitation(strings.NewReader(engineInvited), "tester")
	if err != nil {
		t.Fatalf("reading what the engine printed: %v", err)
	}

	if !strings.HasPrefix(invitation.Link, "esper://join/") {
		t.Errorf("the link is %q", invitation.Link)
	}
	// The link is the secret and the whole of it has to survive: half a link
	// is an invitation nobody can use, and the failure is at the far end.
	if invitation.Link != "esper://join/AZu9yNmY-3a4GuP2Idvm1QS5P_1lj8DRn7br382eE6E9q18cWCU8qxLqLfBjoi799HERXh3i-a_rCkDG6CuXAEgAAAAAaqQ-OQ4xMjcuMC4wLjE6NzQ0MwlSM1FILVBHUlI" {
		t.Errorf("the link came back as %q, %d characters of the %d the engine printed",
			invitation.Link, len(invitation.Link), len("esper://join/AZu9yNmY-3a4GuP2Idvm1QS5P_1lj8DRn7br382eE6E9q18cWCU8qxLqLfBjoi799HERXh3i-a_rCkDG6CuXAEgAAAAAaqQ-OQ4xMjcuMC4wLjE6NzQ0MwlSM1FILVBHUlI"))
	}
	if strings.ContainsAny(invitation.Link, " \t") {
		t.Errorf("the link carries whitespace, so it was not read to its end: %q", invitation.Link)
	}

	if invitation.Broker != "127.0.0.1:7443" {
		t.Errorf("broker %q, and the engine printed 127.0.0.1:7443", invitation.Broker)
	}
	if invitation.For != "tester" {
		t.Errorf("made for %q", invitation.For)
	}
	if invitation.State != "offering" {
		t.Errorf("state %q before anybody has taken it up", invitation.State)
	}

	// "expires in 30 minute(s)" is a duration and a screen counts down to an
	// instant, so the reading is turned into one as the line is read.
	expires := time.Unix(invitation.ExpiresAt, 0)
	if expires.Before(before.Add(29*time.Minute)) || expires.After(time.Now().Add(31*time.Minute)) {
		t.Errorf("expires at %v, and the engine said 30 minutes from about %v", expires, before)
	}
}

/*
The refusal is the useful part.

With no broker the engine explains exactly that, in three sentences that say
what to do about it — and a control plane that replaced them with "could not
invite" would leave somebody with a screen that says something failed and no
idea that the answer is to run a broker.
*/
func TestAnEngineWithNoBrokerSaysSoInItsOwnWords(t *testing.T) {
	invitation, err := readInvitation(strings.NewReader(engineHasNoBroker), "tester")
	if err == nil {
		t.Fatalf("no link was printed and an invitation came back: %+v", invitation)
	}
	if !strings.Contains(err.Error(), "broker") {
		t.Errorf("the engine explained a missing broker and the error is %q", err)
	}

	if _, err := readInvitation(strings.NewReader(""), "tester"); err == nil {
		t.Error("an engine that printed nothing produced an invitation")
	}
}

/*
Only a link is a link.

The pattern is the same allowlist the scheme handler holds — base64url, and at
least sixteen characters of it — so a sentence that merely mentions the scheme
is not mistaken for an invitation. Without that, the engine's own help text
("run `esper://join/<link>`") would be read as the thing it is describing.
*/
func TestSomethingThatIsNotALinkIsNotReadAsOne(t *testing.T) {
	for _, said := range []string{
		"  They run:  esper-engine join esper://join/<the link>\n",
		"  see esper://join/short\n",
		"  esper://join/\n",
		"  https://example.test/join/AZu9yNmY3a4GuP2Idvm1QS5P1lj8DRn7br382eE6E\n",
	} {
		invitation, err := readInvitation(strings.NewReader(said), "tester")
		if err == nil {
			t.Errorf("%q was read as an invitation: %+v", strings.TrimSpace(said), invitation)
		}
	}
}

/*
What the screen polls while an invitation is out.

The same rule as pairing's: the handler reads this while the goroutine running
the engine is still writing to it, so what comes back is a copy. A caller that
kept the pointer could change what this machine believes it has offered.
*/
func TestAnInvitationSnapshotIsACopy(t *testing.T) {
	var i inviting
	if i.snapshot() != nil {
		t.Fatal("a machine that has invited nobody produced an invitation")
	}

	i.set(&Invitation{Link: "esper://join/abcdefghijklmnop", For: "tester", State: "offering"})
	seen := i.snapshot()
	if seen == nil {
		t.Fatal("nothing came back from a machine that is inviting")
	}
	seen.Link = "esper://join/somebody-elses"
	seen.State = "paired"
	if again := i.snapshot(); again.Link != "esper://join/abcdefghijklmnop" || again.State != "offering" {
		t.Errorf("changing a snapshot changed the invitation: %+v", again)
	}

	i.finish("failed", "the broker refused")
	after := i.snapshot()
	if after.State != "failed" || after.Detail != "the broker refused" {
		t.Errorf("finish did not reach the invitation: %+v", after)
	}
	if after.For != "tester" {
		t.Errorf("who it was for was lost when it finished: %+v", after)
	}

	// Ending one that was never started, twice, from the handler and from the
	// goroutine unwinding behind it.
	var fresh inviting
	fresh.stop()
	fresh.stop()
	fresh.finish("expired", "")
	if fresh.snapshot() != nil {
		t.Error("finishing without an invitation invented one")
	}
}
