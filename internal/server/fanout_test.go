package server

import (
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/elythi0n/esper/internal/proto"
)

/*
within fails rather than hangs.

Both properties here are about something *not* blocking, and the natural way to
write that is to do the thing and see whether the test finishes — which turns a
regression into a suite that hangs until Go's ten-minute timeout and then prints
every goroutine in the process. A bounded wait says which claim broke, in a
second, in one line.
*/
func within(t *testing.T, d time.Duration, what string, run func()) {
	t.Helper()
	done := make(chan struct{})
	go func() {
		defer close(done)
		run()
	}()
	select {
	case <-done:
	case <-time.After(d):
		t.Fatalf("%s did not finish within %s", what, d)
	}
}

/*
What happens to open streams when the control plane stops.

Every `/stream` request is a goroutine ranging over a channel. If shutdown
leaves those channels open, the process cannot exit: each reader waits for a
line that will never come, and the server waits for the readers. So closing the
fanout closes every channel, and a reader that is ranging simply ends.

The second half matters as much: a request that arrives *during* shutdown has
to get a channel that is already closed rather than one that never closes. A
handler holding an open channel nobody will ever write to is the same hang
arriving a moment later.
*/
func TestClosingTheFanoutEndsEveryReader(t *testing.T) {
	f := newFanout()

	ids := make([]int, 0, 3)
	channels := make([]chan proto.Happening, 0, 3)
	for range 3 {
		id, ch, err := f.join()
		if err != nil {
			t.Fatalf("joining: %v", err)
		}
		ids = append(ids, id)
		channels = append(channels, ch)
	}

	// Every reader ends when the fanout does, and not before.
	var waiting sync.WaitGroup
	for _, ch := range channels {
		waiting.Add(1)
		go func(ch chan proto.Happening) {
			defer waiting.Done()
			for range ch {
			}
		}(ch)
	}

	f.publish(proto.Happening{Key: "019a1125", Detail: "paired with elythion"})
	f.close()
	within(t, 5*time.Second, "the readers of a closed fanout", waiting.Wait)

	// Joining afterwards gets a channel that has already ended, so a request
	// arriving mid-shutdown finishes rather than waiting for a line.
	id, ch, err := f.join()
	if err != nil {
		t.Fatalf("a stream joining after shutdown was refused rather than ended: %v", err)
	}
	if id != -1 {
		t.Errorf("a stream that joined after shutdown was given id %d", id)
	}
	if _, open := <-ch; open {
		t.Error("a stream that joined after shutdown was given an open channel")
	}

	// Closing twice is what a shutdown path that runs twice does, and it is
	// not a panic on a closed channel.
	f.close()

	// And leaving a stream that was already closed out from under it is the
	// ordinary race between a reader going away and the server stopping.
	f.leave(ids[0])
	f.leave(9999)

	// Publishing to nobody is not a failure either: the last reader can go
	// away between a session ending and the line being written.
	f.publish(proto.Happening{Key: "019a1125"})
}

/*
A reader that stops draining loses lines; it does not stop the server.

The lines are a notification and `/sessions` is the record, so a stream nobody
is reading drops rather than grows — otherwise one paused browser tab is a
control plane whose memory climbs until something kills it.
*/
// **There is a ceiling on open streams, and it is said out loud.**
//
// A browser limits connections per origin; a program holding the token does
// not, and every stream is a goroutine, a buffer and a slot `publish` walks
// while holding the lock. Without a ceiling, enough of them make every session
// line slow for everybody — and nothing in the product would say why.
func TestOnlySoManyStreamsMayBeOpenAtOnce(t *testing.T) {
	f := newFanout()
	defer f.close()

	for i := range atMost {
		if _, _, err := f.join(); err != nil {
			t.Fatalf("stream %d of %d was refused: %v", i+1, atMost, err)
		}
	}
	_, ch, err := f.join()
	if err == nil {
		t.Fatal("a stream past the ceiling was let in, so there is no ceiling")
	}
	if ch != nil {
		t.Error("a refused stream was given a channel to read anyway")
	}
	// The refusal names the limit and what to do, because "service
	// unavailable" on a machine that is plainly running is not an answer.
	if !strings.Contains(err.Error(), strconv.Itoa(atMost)) {
		t.Errorf("the refusal does not say how many is too many: %v", err)
	}

	// And a stream that leaves makes room, so this is a ceiling and not a
	// budget spent once.
	f.leave(0)
	if _, _, err := f.join(); err != nil {
		t.Errorf("nothing could join after a stream left: %v", err)
	}
}

// A line published while the fanout is full still reaches everybody in it: the
// ceiling refuses new readers and changes nothing about the ones already
// reading.
func TestAFullFanoutStillDeliversToEveryoneInIt(t *testing.T) {
	f := newFanout()
	defer f.close()

	channels := make([]chan proto.Happening, 0, atMost)
	for range atMost {
		_, ch, err := f.join()
		if err != nil {
			t.Fatalf("joining: %v", err)
		}
		channels = append(channels, ch)
	}
	if _, _, err := f.join(); err == nil {
		t.Fatal("the fanout is not full, so this measures nothing")
	}

	f.publish(proto.Happening{Key: "019a1125", At: 7})
	for i, ch := range channels {
		select {
		case got := <-ch:
			if got.At != 7 {
				t.Errorf("stream %d received %+v", i, got)
			}
		default:
			t.Errorf("stream %d received nothing while the fanout was full", i)
		}
	}
}

func TestASlowReaderLosesLinesRatherThanHoldingThemUp(t *testing.T) {
	f := newFanout()
	defer f.close()

	_, ch, err := f.join()
	if err != nil {
		t.Fatalf("joining: %v", err)
	}
	within(t, 5*time.Second, "publishing to a stream nobody is draining", func() {
		for i := range deep * 4 {
			f.publish(proto.Happening{Key: "019a1125", At: uint64(i)})
		}
	})

	if len(ch) != deep {
		t.Errorf("a stream nobody drained holds %d lines, and the bound is %d", len(ch), deep)
	}

	// What it kept is the *oldest* of them, which is what a bounded channel
	// does — worth stating, because "drops the newest" and "drops the oldest"
	// are both defensible and only one of them is what happens.
	first := <-ch
	if first.At != 0 {
		t.Errorf("the first line held is %d, and a full channel drops what arrives after it", first.At)
	}
}
