package main

import (
	"io"
	"regexp"
	"strings"
	"testing"
	"time"
)

/*
The child's output keeps being read after the line that was waited for.

Two things went wrong when it stopped there. A host writes a line for every
permission, every screen and every refusal, so a process whose output nobody
reads blocks once the kernel's pipe buffer fills — which from here looks like
a host that hung. And everything the host says about *why it stopped* comes
after "listening", so a diagnostic whose whole purpose is to report the host's
own reason was throwing that reason away.
*/
func TestTheChildIsStillReadAfterTheLineWeWaitedFor(t *testing.T) {
	said, write := io.Pipe()
	watching := watch(said, regexp.MustCompile(`listening\s+(\S+:\d+)`))

	go func() {
		_, _ = io.WriteString(write, "  starting\n  listening 127.0.0.1:5555\n")
		// Written only after the match, which is where the reasons live.
		_, _ = io.WriteString(write, "  indicator  NOT on screen: something is drawn over it\n")
		_ = write.Close()
	}()

	line := watching.waitFor(nil)
	if !strings.Contains(line, "127.0.0.1:5555") {
		t.Fatalf("the line it waited for is %q", line)
	}

	// The reader is a goroutine, so the later line arrives when it arrives.
	var seen []string
	for waited := 0; waited < 200; waited++ {
		seen = watching.lines()
		if len(seen) >= 3 {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	if !strings.Contains(strings.Join(seen, "\n"), "NOT on screen") {
		t.Fatalf("what the child said after the match was lost: %q", seen)
	}
}

// And a child that says a great deal is not held up by nobody reading it.
//
// The pipe here is unbuffered, so a writer whose reader has stopped blocks on
// the first write past it — which is exactly the shape of the fault.
func TestALoudChildIsNotBlockedByTheReader(t *testing.T) {
	said, write := io.Pipe()
	watching := watch(said, regexp.MustCompile(`listening`))

	done := make(chan struct{})
	go func() {
		defer close(done)
		_, _ = io.WriteString(write, "listening 127.0.0.1:1\n")
		for i := 0; i < 5000; i++ {
			if _, err := io.WriteString(write, "  a line the host printed after it started\n"); err != nil {
				return
			}
		}
		_ = write.Close()
	}()

	watching.waitFor(nil)
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("the child was still writing after five seconds — nothing is draining its output")
	}

	// And what is kept is bounded: a session that ran for an hour must not be
	// held in memory by a diagnostic.
	if kept := len(watching.lines()); kept > mostLinesKept {
		t.Fatalf("it kept %d lines, and the bound is %d", kept, mostLinesKept)
	}
}
