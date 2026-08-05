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

func newFanout() *fanout {
	return &fanout{streams: make(map[int]chan proto.Happening)}
}

func (f *fanout) join() (int, chan proto.Happening) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.closed {
		ch := make(chan proto.Happening)
		close(ch)
		return -1, ch
	}
	id := f.next
	f.next++
	ch := make(chan proto.Happening, deep)
	f.streams[id] = ch
	return id, ch
}

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

	id, lines := s.events.join()
	defer s.events.leave(id)
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
