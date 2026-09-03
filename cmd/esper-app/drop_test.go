package main

import (
	"errors"
	"os/exec"
	"strings"
	"testing"
	"time"
)

/*
What the window keeps about a file on its way.

The transfer itself is the engine's — `cp` has its own session, its own
permission and its own tests. What is here is the bookkeeping a screen reads,
and the two things it can get wrong: losing the order, and keeping everything
for ever.
*/
func TestWhatAWindowSaysItIsSending(t *testing.T) {
	var sending transfers
	first := sending.begin("notes.txt")
	second := sending.begin("a photo.png")

	shown := sending.list()
	if len(shown) != 2 {
		t.Fatalf("two files were dropped and the window lists %d", len(shown))
	}
	// Oldest first: a list that reordered itself while somebody was reading it
	// would move the line they were looking at.
	if shown[0].Name != "notes.txt" || shown[1].Name != "a photo.png" {
		t.Fatalf("out of order: %q then %q", shown[0].Name, shown[1].Name)
	}
	if shown[0].State != "sending" {
		t.Fatalf("a file just dropped is %q, and it is being sent", shown[0].State)
	}

	sending.finish(first, nil)
	sending.finish(second, errors.New("the host does not accept files"))
	shown = sending.list()
	if shown[0].State != "sent" {
		t.Fatalf("a finished transfer is %q", shown[0].State)
	}
	if shown[1].State != "failed" || shown[1].Trouble != "the host does not accept files" {
		t.Fatalf("a failed transfer says %q: %q", shown[1].State, shown[1].Trouble)
	}
}

// A finished transfer goes away, and one still running never does.
//
// Without this the list is a session's whole history, growing for as long as
// the window is open — and the newest thing, which is the one somebody is
// looking for, is at the bottom of it.
func TestFinishedTransfersAreLetGoOf(t *testing.T) {
	var sending transfers
	old := sending.begin("long ago.txt")
	sending.finish(old, nil)
	old.done = time.Now().Add(-2 * transferShownFor)

	running := sending.begin("still going.iso")
	// Long past the window, and still not finished: a slow transfer is not an
	// old one, and dropping it would take the line that says so off the
	// screen.
	running.started = time.Now().Add(-4 * transferShownFor)

	shown := sending.list()
	if len(shown) != 1 || shown[0].Name != "still going.iso" {
		t.Fatalf("the list kept %d: %+v", len(shown), shown)
	}
}

/*
What a failure says, which is the host's own sentence where there is one.

The engine prints what it was doing and then what happened, so the last line
is the answer and the ones before it are the transcript. A screen has room for
the answer.
*/
func TestAFailureSaysTheLastThingTheEngineSaid(t *testing.T) {
	said := "📄 esper cp\n\n  reached    127.0.0.1:8080\n  refused    the host does not allow files in\n"
	if got := lastSentence(said); got != "refused    the host does not allow files in" {
		t.Fatalf("the reason read %q", got)
	}
	if got := lastSentence("   \n\n  "); got != "the transfer failed and said nothing" {
		t.Fatalf("a silent failure read %q", got)
	}
}

/*
Which device a drop goes to: the one this window is showing, and nothing else.

A drop on a window with no session has nowhere to go, and a window that
guessed — the last device, the first paired one — would send somebody's file
to a machine they are not looking at.
*/
func TestADropGoesToTheSessionThisWindowIsShowing(t *testing.T) {
	var sessions running
	if sessions.showing() != "" {
		t.Fatalf("a window with no session says it is showing %q", sessions.showing())
	}

	// A session, as far as this bookkeeping is concerned: something running,
	// and the device it was started for.
	held := exec.Command("sleep", "5")
	if err := held.Start(); err != nil {
		t.Skipf("nothing to hold a session open with: %v", err)
	}
	t.Cleanup(func() {
		_ = held.Process.Kill()
		_ = held.Wait()
	})
	sessions.cmd = held
	sessions.device = "827aabec450ddf13"
	if sessions.showing() != "827aabec450ddf13" {
		t.Fatalf("a window with a session says it is showing %q", sessions.showing())
	}

	sessions.stop()
	if sessions.showing() != "" {
		t.Fatalf("a window whose session ended says it is showing %q", sessions.showing())
	}
}

// And the device is still a fingerprint by the time it reaches a command line.
//
// `send` puts it there directly, so the allowlist that guards `start` has to
// be the same one guarding this — which it is, because `showing` can only ever
// return what `start` accepted.
func TestOnlyAStartedDeviceCanBeShowing(t *testing.T) {
	var sessions running
	if err := sessions.start("not a fingerprint", "/tmp/socket"); err == nil {
		t.Fatal("a device that is not a fingerprint was accepted")
	} else if !strings.Contains(err.Error(), "that is not a fingerprint") {
		t.Fatalf("refused for the wrong reason: %v", err)
	}
	if sessions.showing() != "" {
		t.Fatalf("a refused device is being shown as %q", sessions.showing())
	}
}
