package main

import (
	"errors"
	"os"
	"os/exec"
	"path/filepath"
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

// The figure the page shows comes off the engine's own output.
//
// Two things make this easy to get wrong. The engine reports many times, so
// the *last* pair is the current one; and a pipe breaks where it likes, so a
// pair can arrive in two pieces. Reading the accumulated tail handles both,
// and this is what says so.
func TestProgressIsReadFromTheEnginesOutput(t *testing.T) {
	if _, _, ok := progressOf("  sending    big.iso → elythion:big.iso (900 bytes)\n"); ok {
		t.Fatal("a line with no progress in it was read as progress")
	}

	sent, total, ok := progressOf("  progress   256 of 900 bytes\n")
	if !ok || sent != 256 || total != 900 {
		t.Fatalf("one report read as %d of %d (%v)", sent, total, ok)
	}

	// Many reports: the newest is the one somebody is looking at.
	many := "  progress   256 of 900 bytes\n  progress   512 of 900 bytes\n  progress   900 of 900 bytes\n"
	if sent, total, ok = progressOf(many); !ok || sent != 900 || total != 900 {
		t.Fatalf("the last of three reports read as %d of %d (%v)", sent, total, ok)
	}

	// The same output, split where a pipe might split it. Every prefix has to
	// give the newest complete pair in it and never a mangled one.
	for cut := 0; cut <= len(many); cut++ {
		head := many[:cut]
		sent, total, ok = progressOf(head)
		if !ok {
			continue
		}
		if total != 900 {
			t.Fatalf("cut at %d read a total of %d", cut, total)
		}
		if sent != 256 && sent != 512 && sent != 900 {
			t.Fatalf("cut at %d read %d bytes sent, which was never reported", cut, sent)
		}
	}
}

// A finished transfer is all of it, whatever the last report said.
func TestAFinishedTransferIsWhole(t *testing.T) {
	var queue transfers
	item := queue.begin("big.iso")
	queue.progress(item, 512, 900)
	queue.finish(item, nil)

	list := queue.list()
	if len(list) != 1 {
		t.Fatalf("the queue holds %d transfers", len(list))
	}
	if list[0].State != "sent" || list[0].Bytes != 900 || list[0].Total != 900 {
		t.Fatalf("a sent file reads as %q at %d of %d", list[0].State, list[0].Bytes, list[0].Total)
	}

	// A failure keeps the figure it stopped at: rounding it up to the total
	// would say a file arrived that did not.
	failed := queue.begin("other.iso")
	queue.progress(failed, 512, 900)
	queue.finish(failed, errors.New("the link went"))
	list = queue.list()
	last := list[len(list)-1]
	if last.State != "failed" || last.Bytes != 512 {
		t.Fatalf("a failed file reads as %q at %d bytes", last.State, last.Bytes)
	}
}

/*
The whole path, with a stand-in for the engine.

`send` builds a command line, reads what comes back as it comes, and turns it
into the figures a page shows. Every part of that has been wrong at some point
— the flag missing, the pipe read only at the end, the pair split across a
read — and none of it is visible from the outside of this window except
through what the queue says. So the engine is replaced by a script that prints
what the real one prints, and the claim is about the queue afterwards.
*/
func TestSendReadsTheEnginesProgressAsItArrives(t *testing.T) {
	front := filepath.Join(t.TempDir(), "esper")
	script := `#!/bin/sh
echo "  sending    big.iso → elythion:big.iso (900 bytes)" >&2
# The flag the window has to pass, or the real engine says nothing at all.
case " $* " in *" --progress "*) ;; *) echo "no --progress" >&2; exit 3;; esac
echo "  progress   256 of 900 bytes" >&2
echo "  progress   512 of 900 bytes" >&2
echo "  progress   900 of 900 bytes" >&2
echo "  sent       big.iso" >&2
`
	if err := os.WriteFile(front, []byte(script), 0o700); err != nil {
		t.Fatalf("could not write the stand-in engine: %v", err)
	}

	var queue transfers
	queue.send(front, "827aabec450ddf13", filepath.Join(t.TempDir(), "big.iso"))

	list := queue.list()
	if len(list) != 1 {
		t.Fatalf("the queue holds %d transfers", len(list))
	}
	got := list[0]
	if got.State != "sent" {
		t.Fatalf("a copy that worked reads as %q: %s", got.State, got.Trouble)
	}
	if got.Bytes != 900 || got.Total != 900 {
		t.Fatalf("the page is told %d of %d bytes", got.Bytes, got.Total)
	}
	if got.Name != "big.iso" {
		t.Fatalf("the transfer is named %q", got.Name)
	}
}

// And a copy that fails carries the engine's own last sentence, from either
// stream — the window reads both, because losing whichever one moved is how a
// failure becomes "it did not work".
func TestSendKeepsTheEnginesReasonFromEitherStream(t *testing.T) {
	for _, where := range []struct {
		what     string
		redirect string
	}{
		{"standard error", ">&2"},
		{"standard output", ""},
	} {
		front := filepath.Join(t.TempDir(), "esper")
		script := "#!/bin/sh\necho \"  the host does not allow files in\" " + where.redirect + "\nexit 1\n"
		if err := os.WriteFile(front, []byte(script), 0o700); err != nil {
			t.Fatalf("could not write the stand-in engine: %v", err)
		}
		var queue transfers
		queue.send(front, "827aabec450ddf13", filepath.Join(t.TempDir(), "big.iso"))
		list := queue.list()
		if len(list) != 1 || list[0].State != "failed" {
			t.Fatalf("a copy that failed on %s reads as %v", where.what, list)
		}
		if !strings.Contains(list[0].Trouble, "does not allow files in") {
			t.Fatalf("the reason from %s is %q", where.what, list[0].Trouble)
		}
	}
}

/*
And it is read *while* the copy is running, not collected at the end.

This is the property a finished transfer cannot show: the figures are right
either way once the process exits. `CombinedOutput` waits for the child, so a
window using it knows nothing for the whole time somebody is watching a large
file move — which is the entire reason the page has a percentage.

The stand-in prints its progress and then waits, so the only way the queue can
report anything is if the window is reading as it goes.
*/
func TestTheQueueMovesWhileTheCopyIsStillRunning(t *testing.T) {
	dir := t.TempDir()
	front := filepath.Join(dir, "esper")
	gate := filepath.Join(dir, "carry-on")
	script := "#!/bin/sh\n" +
		"echo \"  progress   256 of 900 bytes\" >&2\n" +
		"while [ ! -e " + gate + " ]; do sleep 0.05; done\n" +
		"echo \"  progress   900 of 900 bytes\" >&2\n"
	if err := os.WriteFile(front, []byte(script), 0o700); err != nil {
		t.Fatalf("could not write the stand-in engine: %v", err)
	}

	var queue transfers
	done := make(chan struct{})
	go func() {
		defer close(done)
		queue.send(front, "827aabec450ddf13", filepath.Join(dir, "big.iso"))
	}()

	// What the page would see, while the copy is still going.
	moved := false
	for waited := 0; waited < 200 && !moved; waited++ {
		for _, item := range queue.list() {
			if item.State == "sending" && item.Bytes == 256 && item.Total == 900 {
				moved = true
			}
		}
		if !moved {
			time.Sleep(10 * time.Millisecond)
		}
	}
	if err := os.WriteFile(gate, nil, 0o600); err != nil {
		t.Fatalf("could not let the copy finish: %v", err)
	}
	<-done

	if !moved {
		t.Fatal("the queue never showed a partly-sent file — the output is being read only at the end")
	}
}
