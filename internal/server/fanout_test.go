package server

import (
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
		id, ch := f.join()
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
	id, ch := f.join()
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
func TestASlowReaderLosesLinesRatherThanHoldingThemUp(t *testing.T) {
	f := newFanout()
	defer f.close()

	_, ch := f.join()
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
