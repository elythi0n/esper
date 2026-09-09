package server

import (
	"encoding/json"
	"fmt"
	"net/http"
	"sync"

	"github.com/elythi0n/esper/internal/proto"
)

// fanout hands one session line to every open stream.
type fanout struct {
	mu      sync.Mutex
	next    int
	streams map[int]chan proto.Happening
	closed  bool
}

// deep is how many lines one slow reader may fall behind before it loses them.
//
// **Dropped rather than buffered without limit.** A stream nobody is draining
// must not be able to grow this process's memory; and the lines are a
// notification, not the record — `/sessions` is the record, and it is a file.
const deep = 32

// atMost is how many streams may be open at once.
//
// **Bounded, because nothing else bounds it.** A browser limits connections
// per origin; a program holding the token does not, and every open stream is a
// goroutine, a buffer and a slot `publish` walks while holding the lock — so
// enough of them make every session line slow for everybody.
//
// Generous against what actually opens one: the window opens exactly one, and
// a second tool, a terminal watching, and a dashboard are three more. Sixty
// four is room for every honest caller and a ceiling for a runaway one.
const atMost = 64

func newFanout() *fanout {
	return &fanout{streams: make(map[int]chan proto.Happening)}
}

// join opens a stream, or reports that there is no room for one.
//
// A closed fanout hands back a channel that is already closed rather than a
// refusal: the reader's loop ends on the first receive, which is what a
// shutting-down server should look like to somebody already connected.
func (f *fanout) join() (int, chan proto.Happening, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.closed {
		ch := make(chan proto.Happening)
		close(ch)
		return -1, ch, nil
	}
	if len(f.streams) >= atMost {
		return -1, nil, errTooManyStreams
	}
	id := f.next
	f.next++
	ch := make(chan proto.Happening, deep)
	f.streams[id] = ch
	return id, ch, nil
}

// errTooManyStreams is what a caller opening one too many is told.
var errTooManyStreams = fmt.Errorf(
	"this machine is already sending %d event streams, which is as many as it keeps open. "+
		"One is enough for a window: close the ones that are no longer read, or ask again",
	atMost,
)

func (f *fanout) leave(id int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if ch, ok := f.streams[id]; ok {
		delete(f.streams, id)
		close(ch)
	}
}

func (f *fanout) publish(h proto.Happening) {
	f.mu.Lock()
	defer f.mu.Unlock()
	for _, ch := range f.streams {
		select {
		case ch <- h:
		default:
			// See `deep`.
		}
	}
}

func (f *fanout) close() {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.closed {
		return
	}
	f.closed = true
	for id, ch := range f.streams {
		delete(f.streams, id)
		close(ch)
	}
}

// stream is the SSE route: session lines as they happen.
//
// **A notification, not a replay.** Nothing is sent for what happened before
// the stream opened: the record is `/sessions`, and a stream that tried to be
// both would be a second, worse copy of a file that already exists.
func (s *Server) stream(w http.ResponseWriter, r *http.Request) {
	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "this server cannot stream", http.StatusInternalServerError)
		return
	}
	// **Joined before a single header goes out.** A refusal after the 200 is a
	// stream every SSE client believes opened, and then a body it cannot read
	// as an event — so the one thing that can be refused here is refused
	// first, with a status that says to try again.
	id, lines, err := s.events.join()
	if err != nil {
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
		return
	}
	defer s.events.leave(id)

	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.WriteHeader(http.StatusOK)
	// **Said before anything happens**, so a caller knows the stream is open
	// rather than waiting to find out from the first session.
	// A write that fails means the caller has gone; there is nothing to
	// report it to, and returning is the whole of the correct response.
	if _, err := fmt.Fprint(w, ": open\n\n"); err != nil {
		return
	}
	flusher.Flush()

	for {
		select {
		case <-r.Context().Done():
			return
		case line, open := <-lines:
			if !open {
				return
			}
			body, err := json.Marshal(line)
			if err != nil {
				continue
			}
			if _, err := fmt.Fprintf(w, "event: session\ndata: %s\n\n", body); err != nil {
				// The reader has gone. Its stream goes with it.
				return
			}
			flusher.Flush()
		}
	}
}
